//! Brace-free in-segment matcher — the `WildKind::Pieces` backend. A
//! segment of literals, `?`s and classes between `*`s (`a*b?[c-e]*f`)
//! is the pieces its `*`s cut, each of a fixed length. The first is
//! matched at the segment's start, the last at its end, and each one
//! between at its leftmost place after the one before: that leaves the
//! most room to the rest, so no match is missed and no place is tried
//! twice. A piece up to `MAX_SCAN` positions is tried at each place in
//! turn; a longer one is found by shift-and, a word operation per 64
//! positions a byte, so a segment costs O(n·m/64) at worst.
//! JS twin: `packages/globstar/src/engine/segment/pieces.js`.

use std::collections::HashMap;

use crate::bytes::fold;
use crate::parse::ast::Node;

use super::elem::NEVER;
use super::seg_nfa::ByteSet;
use super::wild::{Wild, WildKind};

/// A piece up to this many positions is tried at each place in turn, at
/// most this many byte tests a place; a longer one is searched by
/// shift-and.
const MAX_SCAN: usize = 32;

#[derive(Debug, Clone)]
pub(super) struct Pieces {
    /// Every piece's positions, piece after piece.
    at: Box<[Pos]>,
    /// Where each piece ends in `at`: piece `k` is
    /// `at[ends[k - 1]..ends[k]]`, the first from 0.
    ends: Box<[u32]>,
    /// The classes the positions name, each once.
    classes: Box<[ByteSet]>,
    /// The shift-and tables of the pieces between the first and the
    /// last longer than `MAX_SCAN`, in order.
    long: Box<[ShiftAnd]>,
}

/// What a position takes: a byte (ASCII-lowercased when
/// case-insensitive), any byte (`?`), or a class of `Pieces::classes`.
#[derive(Debug, Clone, Copy)]
enum Pos {
    Byte(u8),
    Any,
    Class(u16),
}

impl Pos {
    #[inline(always)]
    fn takes(self, b: u8, classes: &[ByteSet], ci: bool) -> bool {
        match self {
            Pos::Byte(x) => fold(b, ci) == x,
            Pos::Any => true,
            Pos::Class(k) => classes[usize::from(k)].has(b),
        }
    }
}

impl Pieces {
    /// The segment as a `Wild` over its pieces, or `None` when it holds a
    /// brace (an NFA's). What the graph reads off it is what the NFA's
    /// would be: `NEVER` when a class takes no byte, and protected
    /// unless it starts with a literal `.` or a positive class listing
    /// it; its wildcards alone take no segment, as a literal or a class
    /// stands in each (only `?`s and `*`s are an `Affix`).
    pub(super) fn wild(nodes: &[&Node], dot: bool, ci: bool) -> Option<Wild> {
        if !nodes.iter().all(|n| {
            matches!(
                n,
                Node::Literal(_) | Node::AnyChar | Node::Star | Node::Class(_)
            )
        }) {
            return None;
        }
        let mut at = Vec::new();
        let mut ends = Vec::new();
        let mut classes: Vec<ByteSet> = Vec::new();
        let mut index: HashMap<[u64; 4], u16> = HashMap::new();
        let mut star = false;
        for node in nodes {
            match node {
                Node::Literal(bytes) => at.extend(bytes.iter().map(|&b| Pos::Byte(fold(b, ci)))),
                Node::AnyChar => at.push(Pos::Any),
                Node::Class(cls) => {
                    let set = ByteSet::of(cls, ci);
                    let k = match index.get(&set.bits) {
                        Some(&k) => k,
                        None => {
                            let k = u16::try_from(classes.len()).ok()?;
                            index.insert(set.bits, k);
                            classes.push(set);
                            k
                        }
                    };
                    at.push(Pos::Class(k));
                }
                // A run of `*`s is one: no empty piece between.
                Node::Star if star => continue,
                Node::Star => ends.push(at.len() as u32),
                _ => unreachable!("not a piece's node"),
            }
            star = matches!(node, Node::Star);
        }
        ends.push(at.len() as u32);
        let can_lit_dot = match (nodes[0], at.first()) {
            (Node::Literal(bytes), _) => bytes[0] == b'.',
            (Node::Class(cls), Some(&Pos::Class(k))) => {
                !cls.negated && classes[usize::from(k)].has(b'.')
            }
            _ => false,
        };
        let min_len = if classes.iter().any(ByteSet::is_empty) {
            NEVER
        } else {
            at.len() as u32
        };
        let mut pieces = Pieces {
            at: at.into_boxed_slice(),
            ends: ends.into_boxed_slice(),
            classes: classes.into_boxed_slice(),
            long: Box::default(),
        };
        let last = pieces.ends.len() - 1;
        pieces.long = (1..last)
            .map(|k| pieces.piece(k))
            .filter(|p| p.len() > MAX_SCAN)
            .map(|p| ShiftAnd::new(p, &pieces.classes, ci))
            .collect();
        Some(Wild {
            min_len,
            variable: last > 0,
            dot_protect: !dot && !can_lit_dot,
            any_seg: false,
            kind: WildKind::Pieces(Box::new(pieces)),
        })
    }

    #[inline]
    fn piece(&self, k: usize) -> &[Pos] {
        let from = if k == 0 { 0 } else { self.ends[k - 1] as usize };
        &self.at[from..self.ends[k] as usize]
    }

    /// Does `seg` match, its length checked against the pieces' (at least
    /// theirs, or theirs alone without a `*`)? The last piece first:
    /// where names differ most.
    #[inline(never)]
    pub(super) fn matches(&self, seg: &[u8], ci: bool) -> bool {
        let last = self.ends.len() - 1;
        let first = self.piece(0);
        if last == 0 {
            return self.fits(first, seg, ci);
        }
        let end = self.piece(last);
        let to = seg.len() - end.len();
        if !self.fits(end, &seg[to..], ci) || !self.fits(first, &seg[..first.len()], ci) {
            return false;
        }
        let mut from = first.len();
        let mut long = self.long.iter();
        for k in 1..last {
            let piece = self.piece(k);
            let found = if piece.len() > MAX_SCAN {
                long.next().unwrap().find(seg, from, to)
            } else {
                self.scan(piece, seg, from, to, ci)
            };
            match found {
                Some(end) => from = end,
                None => return false,
            }
        }
        true
    }

    /// Do the positions take the bytes, the last first?
    #[inline(always)]
    fn fits(&self, piece: &[Pos], seg: &[u8], ci: bool) -> bool {
        piece
            .iter()
            .zip(seg)
            .rev()
            .all(|(&p, &b)| p.takes(b, &self.classes, ci))
    }

    /// Where the leftmost place of `piece` within `seg[from..to]` ends,
    /// trying each in turn.
    fn scan(&self, piece: &[Pos], seg: &[u8], from: usize, to: usize, ci: bool) -> Option<usize> {
        let m = piece.len();
        (from..(to + 1).checked_sub(m)?)
            .find(|&k| self.fits(piece, &seg[k..k + m], ci))
            .map(|k| k + m)
    }
}

/// A long piece's shift-and tables: bit `i` of a byte's mask is set when
/// the piece's position `i` takes the byte. Bytes every position takes
/// or refuses alike share a mask, so there are at most 256 and mostly a
/// few.
#[derive(Debug, Clone)]
struct ShiftAnd {
    /// Each byte's mask, by index into `masks`.
    mask_of: Box<[u8; 256]>,
    /// The masks, `words` words each.
    masks: Box<[u64]>,
    words: usize,
    /// The piece's last position, as a bit of the last word.
    top: u64,
}

impl ShiftAnd {
    fn new(piece: &[Pos], classes: &[ByteSet], ci: bool) -> Self {
        // The bytes in parts, split by each position that takes some of
        // a part: a byte or a class once, and `?` takes them all.
        let mut part = [0u8; 256];
        let mut parts = 1;
        let mut seen = vec![false; 256 + classes.len()];
        for &p in piece {
            let key = match p {
                Pos::Byte(x) => usize::from(x),
                Pos::Class(k) => 256 + usize::from(k),
                Pos::Any => continue,
            };
            if std::mem::replace(&mut seen[key], true) || parts == 256 {
                continue;
            }
            let mut split = [u16::MAX; 512];
            let mut count = 0;
            for b in 0..=255u8 {
                let key =
                    usize::from(part[usize::from(b)]) * 2 + usize::from(p.takes(b, classes, ci));
                if split[key] == u16::MAX {
                    split[key] = count;
                    count += 1;
                }
                part[usize::from(b)] = split[key] as u8;
            }
            parts = usize::from(count);
        }
        // A byte that stands for each part.
        let mut stands = [0u8; 256];
        for b in 0..=255u8 {
            stands[usize::from(part[usize::from(b)])] = b;
        }
        let words = piece.len().div_ceil(64);
        let mut masks = vec![0u64; parts * words];
        for (i, &p) in piece.iter().enumerate() {
            let (w, bit) = (i / 64, 1u64 << (i % 64));
            match p {
                // Case-insensitively both cases are in one part: every
                // position takes both or neither.
                Pos::Byte(x) => masks[usize::from(part[usize::from(x)]) * words + w] |= bit,
                _ => {
                    for (q, &b) in stands[..parts].iter().enumerate() {
                        if p.takes(b, classes, ci) {
                            masks[q * words + w] |= bit;
                        }
                    }
                }
            }
        }
        ShiftAnd {
            mask_of: Box::new(part),
            masks: masks.into_boxed_slice(),
            words,
            top: 1 << ((piece.len() - 1) % 64),
        }
    }

    /// Where the leftmost place of the piece within `seg[from..to]` ends.
    fn find(&self, seg: &[u8], from: usize, to: usize) -> Option<usize> {
        let words = self.words;
        // Bit `i`: the piece's first `i + 1` positions take the bytes up
        // to here.
        let mut d = vec![0u64; words];
        for (j, &b) in seg[from..to].iter().enumerate() {
            let at = usize::from(self.mask_of[usize::from(b)]) * words;
            let mut carry = 1;
            for (x, &m) in d.iter_mut().zip(&self.masks[at..at + words]) {
                let next = (*x << 1 | carry) & m;
                carry = *x >> 63;
                *x = next;
            }
            if d[words - 1] & self.top != 0 {
                return Some(from + j + 1);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Pieces, Pos, ShiftAnd};
    use crate::engine::segment::seg_nfa::SegNfa;
    use crate::engine::segment::wild::{Wild, WildKind};
    use crate::parse::ast::Node;

    fn nodes(ast: &Node) -> Vec<&Node> {
        match ast {
            Node::Concat(nodes) => nodes.iter().collect(),
            node => vec![node],
        }
    }

    /// Every string of up to `most` units, the empty one first.
    fn strings(units: &[&str], most: usize) -> Vec<String> {
        let mut out = vec![String::new()];
        let mut from = 0;
        for _ in 0..most {
            let to = out.len();
            for k in from..to {
                for u in units {
                    out.push(format!("{}{u}", out[k]));
                }
            }
            from = to;
        }
        out
    }

    fn takes(w: &Wild, seg: &[u8], ci: bool) -> bool {
        if w.dot_protect && seg.first() == Some(&b'.') {
            return false;
        }
        let (len, need) = (seg.len(), w.min_len as usize);
        match &w.kind {
            WildKind::Pieces(p) => len >= need && (w.variable || len == need) && p.matches(seg, ci),
            WildKind::Generic(nfa) => nfa.matches(seg),
            _ => unreachable!(),
        }
    }

    // Every brace-free segment of a few tokens takes, as pieces, every
    // short name the NFA does, and the graph reads the same off both.
    #[test]
    fn pieces_take_what_the_nfa_does() {
        let tokens = ["a", "B", ".", "?", "*", "[ab]", "[!a]", "[.b]"];
        let names = strings(&["a", "b", ".", "A"], 4);
        for pattern in strings(&tokens, 4).iter().filter(|p| !p.is_empty()) {
            let ast = crate::parse::parse(pattern.as_bytes()).unwrap();
            let nodes = nodes(&ast.body);
            // `**` alone is a globstar.
            if matches!(nodes[..], [Node::Globstar]) {
                continue;
            }
            // Only `?`s and `*`s are an `Affix`, whose lengths the
            // pieces do not tell.
            let bare = nodes
                .iter()
                .all(|n| matches!(n, Node::AnyChar | Node::Star));
            for (dot, ci) in [(false, false), (true, false), (false, true), (true, true)] {
                let nfa = SegNfa::wild(&nodes, dot, ci);
                let Some(pieces) = Pieces::wild(&nodes, dot, ci) else {
                    panic!("{pattern} has no pieces");
                };
                let at = format!("{pattern} dot={dot} ci={ci}");
                assert_eq!(pieces.dot_protect, nfa.dot_protect, "{at}");
                if !bare {
                    assert_eq!(pieces.lens(), nfa.lens(), "{at}");
                    assert_eq!(pieces.any_seg, nfa.any_seg, "{at}");
                }
                assert_eq!(pieces.min_len == u32::MAX, nfa.min_len == u32::MAX, "{at}");
                for name in &names {
                    let name = name.as_bytes();
                    assert_eq!(
                        takes(&pieces, name, ci),
                        takes(&nfa, name, ci),
                        "{at} on {:?}",
                        String::from_utf8_lossy(name)
                    );
                }
            }
        }
    }

    // Shift-and finds the place trying each in turn does.
    #[test]
    fn shift_and_finds_the_leftmost_place() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n) as usize
        };
        for _ in 0..2000 {
            let len = 1 + rnd(150);
            let pattern: String = (0..len)
                .map(|_| ["a", "b", "?", "[ab]", "[!b]", "A"][rnd(6)])
                .collect();
            let ci = rnd(2) == 0;
            let ast = crate::parse::parse(format!("*{pattern}*").as_bytes()).unwrap();
            let Some(Wild {
                kind: WildKind::Pieces(p),
                ..
            }) = Pieces::wild(&nodes(&ast.body), true, ci)
            else {
                panic!("*{pattern}* has no pieces");
            };
            let piece: Vec<Pos> = p.piece(1).to_vec();
            let search = ShiftAnd::new(&piece, &p.classes, ci);
            let seg: Vec<u8> = (0..rnd(400)).map(|_| b"abAB"[rnd(4)]).collect();
            let from = rnd(seg.len() as u64 + 1);
            let to = from + rnd((seg.len() - from) as u64 + 1);
            assert_eq!(
                search.find(&seg, from, to),
                p.scan(&piece, &seg, from, to, ci),
                "*{pattern}* ci={ci} on {:?}[{from}..{to}]",
                String::from_utf8_lossy(&seg)
            );
        }
    }
}
