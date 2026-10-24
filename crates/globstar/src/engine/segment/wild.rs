//! In-segment wildcard matchers: what a segment with a wildcard compiles
//! to (`compile_wild`) and how it takes a path segment
//! (`wild_consumes`): an affix, a suffix set or pieces here, the
//! in-segment NFA in `seg_nfa.rs`. `wild_consumes` is forced inline
//! (see `chain.rs`).

use std::collections::HashMap;

use crate::bytes::{affix_eq, fold};
use crate::parse::ast::Node;

use super::bits::{ByteSet, ByteSketch};
use super::seg_nfa::SegNfa;
use super::{Bytes, SegmentMatcher};

// ── the wildcard matchers ─────────────────────────────────────────────────

/// The `min_len` of a segment no path segment can fill: one with an
/// escaped `\` on Windows, where a segment never holds a separator, or
/// an in-segment NFA that accepts nothing.
pub(super) const NEVER: u32 = u32::MAX;

#[derive(Debug, Clone)]
pub(super) struct Wild {
    pub(super) kind: WildKind,
    /// Minimum segment byte length (literal parts + `?` count; for
    /// suffix sets excludes the per-branch suffix; `Generic` keeps 0, or
    /// `NEVER` when it accepts nothing).
    pub(super) min_len: u32,
    /// `false` ⇒ length must equal `min_len` exactly (no `*`).
    pub(super) variable: bool,
    /// Reject dot-led segments (wildcard-led matcher on a
    /// `dot=false` compile).
    pub(super) dot_protect: bool,
    /// Its wildcards alone match every non-empty segment (`*`, `?*`,
    /// `{a,*}`); drives `match_dir`'s all-below bit.
    pub(super) any_seg: bool,
}

/// The rarer kinds are boxed so an `Elem` stays 40 bytes.
#[derive(Debug, Clone)]
pub(super) enum WildKind {
    /// `lit (*|?)+ lit` and degenerate forms (`*`, `*lit`, `lit*`,
    /// `a?b`): compare the affixes, the length rules do the rest.
    Affix(Affix),
    /// `lit (*|?)+ {lit,…}` (`*.{ts,tsx}`, a factored union of names):
    /// the prefix, then one suffix out of a set.
    Suffixes(Box<SuffixSet>),
    /// Literals, `?`s and classes between `*`s, no brace
    /// (`a*b*c`, `n*d[k-m]e?txt`): the pieces the `*`s cut, each at its
    /// leftmost place.
    Pieces(Box<Pieces>),
    /// Everything else (a brace around wildcards, classes or a product
    /// past its cap): a mini Thompson NFA over the in-segment nodes.
    Generic(Box<SegNfa>),
}

/// A prefix and a suffix in one string, `split` bytes of prefix.
#[derive(Debug, Clone)]
pub(super) struct Affix {
    bytes: Bytes,
    split: u32,
}

impl Affix {
    pub(super) fn new(prefix: Bytes, suffix: Bytes) -> Self {
        let split = prefix.len() as u32;
        let bytes = match (prefix.is_empty(), suffix.is_empty()) {
            (true, _) => suffix,
            (_, true) => prefix,
            _ => [&prefix[..], &suffix[..]].concat().into_boxed_slice(),
        };
        Affix { bytes, split }
    }

    #[inline]
    pub(super) fn parts(&self) -> (&[u8], &[u8]) {
        self.bytes.split_at(self.split as usize)
    }
}

/// A prefix and a set of suffixes, the suffixes grouped by their last
/// byte: a segment is compared with the few that end as it does,
/// whatever the set's size.
#[derive(Debug, Clone)]
pub(super) struct SuffixSet {
    prefix: Bytes,
    /// The last bytes of the suffixes: a chain's gate, and the short
    /// scan's.
    pub(super) last: ByteSketch,
    /// The non-empty suffixes laid end to end, grouped by last byte,
    /// each group in pattern order; suffix `k` is
    /// `suffixes[bounds[k]..bounds[k + 1]]`.
    suffixes: Bytes,
    bounds: Box<[u32]>,
    /// Past `LINEAR_SUFFIXES`: the group of last byte `b`
    /// (ASCII-lowercased when case-insensitive) is the suffixes
    /// `groups[b & GROUP_MASK]..groups[(b & GROUP_MASK) + 1]`; empty for
    /// fewer, which are scanned in turn.
    groups: Box<[u32]>,
    /// One suffix is empty: it ends anything.
    empty: bool,
}

/// A suffix set up to this size is scanned in turn after its sketch;
/// a larger one by the group of the segment's last byte: its low five
/// bits, which keep ASCII letters apart.
const LINEAR_SUFFIXES: usize = 4;
const GROUPS: usize = 32;
const GROUP_MASK: u8 = GROUPS as u8 - 1;

impl SuffixSet {
    fn new(prefix: Bytes, suffixes: Vec<Vec<u8>>, ci: bool) -> Self {
        let last = ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci);
        let empty = suffixes.iter().any(Vec::is_empty);
        let group = |s: &[u8]| (fold(s[s.len() - 1], ci) & GROUP_MASK) as usize;
        let nonempty = || suffixes.iter().filter(|s| !s.is_empty());
        // Placed by a counting sort on the group, which keeps the
        // pattern's order within one: a pass counts each group's
        // suffixes and bytes, a second copies each suffix to its place.
        let mut groups = [0u32; GROUPS + 1];
        let mut at = [0u32; GROUPS + 1];
        for s in nonempty() {
            let g = group(s) + 1;
            groups[g] += 1;
            at[g] += s.len() as u32;
        }
        for g in 1..=GROUPS {
            groups[g] += groups[g - 1];
            at[g] += at[g - 1];
        }
        let mut bytes = vec![0u8; at[GROUPS] as usize];
        let mut bounds = vec![0u32; groups[GROUPS] as usize + 1];
        let mut next = groups;
        for s in nonempty() {
            let g = group(s);
            let (k, start) = (next[g] as usize, at[g] as usize);
            bytes[start..start + s.len()].copy_from_slice(s);
            bounds[k + 1] = (start + s.len()) as u32;
            next[g] += 1;
            at[g] += s.len() as u32;
        }
        let groups = if groups[GROUPS] as usize <= LINEAR_SUFFIXES {
            Box::default()
        } else {
            Box::from(&groups[..])
        };
        SuffixSet {
            prefix,
            last,
            suffixes: bytes.into_boxed_slice(),
            bounds: bounds.into_boxed_slice(),
            groups,
            empty,
        }
    }

    /// Does a suffix of at most `room` bytes end `seg` (`variable`), or
    /// one of exactly `room` bytes? A few suffixes are compared in turn
    /// after the sketch; of many, only those that end as `seg` does.
    /// Out of line: inlined, it weighed on every other wildcard kind.
    #[inline(never)]
    fn ends(&self, seg: &[u8], room: usize, variable: bool, ci: bool) -> bool {
        let Some(&last) = seg.last() else {
            return false;
        };
        let bounds = if self.groups.is_empty() {
            if !self.last.may_end(seg) {
                return false;
            }
            &self.bounds[..]
        } else {
            let g = (fold(last, ci) & GROUP_MASK) as usize;
            &self.bounds[self.groups[g] as usize..=self.groups[g + 1] as usize]
        };
        for k in 1..bounds.len() {
            let suffix = &self.suffixes[bounds[k - 1] as usize..bounds[k] as usize];
            if suffix.len() <= room
                && (variable || suffix.len() == room)
                && affix_eq(suffix, &seg[seg.len() - suffix.len()..], ci)
            {
                return true;
            }
        }
        false
    }
}

// ── compiling ─────────────────────────────────────────────────────────────

/// The bytes of a run of literal nodes, allocated once.
pub(super) fn concat_lits(nodes: &[&Node]) -> Bytes {
    fn lit(node: &Node) -> &[u8] {
        match node {
            Node::Literal(bytes) => bytes,
            _ => unreachable!("not a literal"),
        }
    }
    match nodes {
        [one] => Bytes::from(lit(one)),
        _ => {
            let mut acc = Vec::with_capacity(nodes.iter().map(|n| lit(n).len()).sum());
            for node in nodes {
                acc.extend_from_slice(lit(node));
            }
            acc.into_boxed_slice()
        }
    }
}

pub(super) fn compile_wild(nodes: &[&Node], dot: bool, ci: bool) -> Wild {
    let mut idx = nodes
        .iter()
        .take_while(|n| matches!(n, Node::Literal(_)))
        .count();
    let prefix = concat_lits(&nodes[..idx]);
    let mut anychars = 0u32;
    let mut has_star = false;
    while idx < nodes.len() {
        match nodes[idx] {
            Node::Star => has_star = true,
            Node::AnyChar => anychars += 1,
            _ => break,
        }
        idx += 1;
    }
    // A literal product after the wildcards (one empty suffix when
    // nothing follows them) keeps the segment anchored at both ends;
    // anything else is pieces, or with a brace runs the NFA.
    let Some(mut suffixes) = suffix_product(&nodes[idx..]) else {
        return Pieces::wild(nodes, dot, ci).unwrap_or_else(|| SegNfa::wild(nodes, dot, ci));
    };
    let dot_protect = !dot && prefix.is_empty() && (has_star || anychars > 0);
    // `*` or `?*` with nothing literal around it, up to a suffix that
    // may be empty.
    let any_seg =
        prefix.is_empty() && has_star && anychars <= 1 && suffixes.iter().any(|s| s.is_empty());
    let mut min_len = prefix.len() as u32 + anychars;
    let kind = if suffixes.len() == 1 {
        let suffix = suffixes.pop().unwrap();
        min_len += suffix.len() as u32;
        WildKind::Affix(Affix::new(prefix, suffix.into_boxed_slice()))
    } else {
        WildKind::Suffixes(Box::new(SuffixSet::new(prefix, suffixes, ci)))
    };
    Wild {
        min_len,
        kind,
        variable: has_star,
        dot_protect,
        any_seg,
    }
}

/// Literal suffix alternatives one segment enumerates (`*.{a,b,…}`, a
/// factored union of names). A product of alternatives (`{a,b}{c,d}…`)
/// past this many suffixes stays an NFA, which holds it without
/// multiplying it out; a single list is no product and is enumerated
/// whatever its length.
const MAX_SUFFIX_PRODUCT: usize = 1024;

/// The enumerated suffixes, one byte more each, may not take more than
/// this or four times their nodes: a long literal next to a product
/// would be copied into every suffix.
const MAX_SUFFIX_BYTES: usize = 64 * 1024;

fn suffix_product(nodes: &[&Node]) -> Option<Vec<Vec<u8>>> {
    if let [Node::Literal(bytes)] = nodes {
        return Some(vec![bytes.clone()]);
    }
    let mut parts: Vec<Vec<u8>> = vec![Vec::new()];
    // Σ (part + 1) bytes, and what the nodes themselves hold.
    let (mut size, mut source) = (1usize, 1usize);
    let fits = |size: usize, source: usize| size <= MAX_SUFFIX_BYTES.max(source.saturating_mul(4));
    for node in nodes {
        match node {
            Node::Literal(bytes) => {
                size = size.saturating_add(parts.len().saturating_mul(bytes.len()));
                source += bytes.len();
                if !fits(size, source) {
                    return None;
                }
                for p in parts.iter_mut() {
                    p.extend_from_slice(bytes);
                }
            }
            Node::Brace(branches) => {
                let mut lits = Vec::with_capacity(branches.len());
                for b in branches {
                    match b.seq() {
                        [] => lits.push(&b""[..]),
                        [Node::Literal(bytes)] => lits.push(bytes.as_slice()),
                        _ => return None,
                    }
                }
                if parts.len() > 1 && parts.len() * lits.len() > MAX_SUFFIX_PRODUCT {
                    return None;
                }
                let branch_bytes: usize = lits.iter().map(|l| l.len() + 1).sum();
                // Σ (|p| + |l| + 1) over every pair.
                size = (lits.len().saturating_mul(size - parts.len()))
                    .saturating_add(parts.len().saturating_mul(branch_bytes));
                source += branch_bytes;
                if !fits(size, source) {
                    return None;
                }
                let mut next = Vec::with_capacity(parts.len() * lits.len());
                for p in &parts {
                    for l in &lits {
                        let mut v = Vec::with_capacity(p.len() + l.len());
                        v.extend_from_slice(p);
                        v.extend_from_slice(l);
                        next.push(v);
                    }
                }
                parts = next;
            }
            _ => return None,
        }
    }
    Some(parts)
}

// ── matching ──────────────────────────────────────────────────────────────

impl SegmentMatcher {
    #[inline(always)]
    pub(super) fn wild_consumes(&self, w: &Wild, seg: &[u8]) -> bool {
        if w.dot_protect && seg.first() == Some(&b'.') {
            return false;
        }
        let len = seg.len();
        match &w.kind {
            WildKind::Affix(affix) => {
                let need = w.min_len as usize;
                if len < need || (!w.variable && len != need) {
                    return false;
                }
                let (prefix, suffix) = affix.parts();
                self.affix_eq(prefix, &seg[..prefix.len()])
                    && self.affix_eq(suffix, &seg[len - suffix.len()..])
            }
            WildKind::Suffixes(set) => {
                // The prefix, then the suffixes that end as the segment
                // does, over what the prefix and the `?`s leave.
                let Some(room) = len.checked_sub(w.min_len as usize) else {
                    return false;
                };
                if !self.affix_eq(&set.prefix, &seg[..set.prefix.len()]) {
                    return false;
                }
                (set.empty && (w.variable || room == 0))
                    || set.ends(seg, room, w.variable, self.case_insensitive)
            }
            WildKind::Pieces(pieces) => {
                let need = w.min_len as usize;
                if len < need || (!w.variable && len != need) {
                    return false;
                }
                pieces.matches(seg, self.case_insensitive)
            }
            WildKind::Generic(nfa) => nfa.matches(seg),
        }
    }
}

// ── pieces ────────────────────────────────────────────────────────────────

// Brace-free in-segment matcher — the `WildKind::Pieces` backend. A
// segment of literals, `?`s and classes between `*`s (`a*b?[c-e]*f`)
// is the pieces its `*`s cut, each of a fixed length. The first is
// matched at the segment's start, the last at its end, and each one
// between at its leftmost place after the one before: that leaves the
// most room to the rest, so no match is missed and no place is tried
// twice. A piece up to `MAX_SCAN` positions is tried at each place in
// turn; a longer one is found by shift-and, a word operation per 64
// positions a byte, so a segment costs O(n·m/64) at worst.

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
    fn wild(nodes: &[&Node], dot: bool, ci: bool) -> Option<Wild> {
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
    fn matches(&self, seg: &[u8], ci: bool) -> bool {
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
    use super::{Pieces, Pos, ShiftAnd, Wild, WildKind};
    use crate::engine::segment::seg_nfa::SegNfa;
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
