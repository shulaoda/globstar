//! In-segment wildcard matchers: what a segment with a wildcard compiles
//! to (`compile_wild`) and how it takes a path segment (`wild_consumes`).

use std::collections::HashMap;

use crate::bytes::{affix_eq, fold};
use crate::parse::ast::Node;

use super::bits::{ByteSet, ByteSketch};
use super::seg_nfa::SegNfa;
use super::{Bytes, SegmentMatcher};

/// The `min_len` of a segment no path segment can fill.
pub(super) const NEVER: u32 = u32::MAX;

/// A suffix set up to this size is scanned in turn after its sketch; a
/// larger one by the group of the segment's last byte, its low five bits,
/// which keep ASCII letters apart.
const LINEAR_SUFFIXES: usize = 4;
const GROUPS: usize = 32;
const GROUP_MASK: u8 = GROUPS as u8 - 1;

/// A product of braces (`{a,b}{c,d}…`) past this many suffixes stays an
/// NFA; a single list is enumerated whatever its length.
const MAX_SUFFIX_PRODUCT: usize = 1024;

/// The enumerated suffixes may not take more than this, or four times
/// their nodes.
const MAX_SUFFIX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub(super) struct Wild {
    pub(super) kind: WildKind,
    /// The literal bytes and `?`s a segment needs; for a suffix set
    /// without the suffix; `NEVER` when nothing fits.
    pub(super) min_len: u32,
    /// Without a `*` the length is exactly `min_len`.
    pub(super) variable: bool,
    /// Rejects a dot-led segment: `dot = false` and nothing takes the `.`
    /// by name.
    pub(super) dot_protect: bool,
    /// Its wildcards alone take every non-empty segment.
    pub(super) any_seg: bool,
}

/// The rarer kinds are boxed, so an `Elem` stays 40 bytes.
#[derive(Debug, Clone)]
pub(super) enum WildKind {
    /// `lit (*|?)+ lit`, any part empty: `*`, `*.ts`, `a?b`.
    Affix(Affix),
    /// `lit (*|?)+ {lit,…}`: `*.{ts,tsx}`, a factored union of names.
    Suffixes(Box<SuffixSet>),
    /// Literals, `?`s and classes between `*`s, no brace: `n*d[k-m]e?txt`.
    Pieces(Box<Pieces>),
    /// Anything else: a brace around wildcards or classes, a product past
    /// its cap.
    Generic(Box<SegNfa>),
}

/// A prefix and a suffix in one string, `split` bytes of prefix.
#[derive(Debug, Clone)]
pub(super) struct Affix {
    bytes: Bytes,
    split: u32,
}

/// A prefix and the suffixes, grouped by their last byte.
#[derive(Debug, Clone)]
pub(super) struct SuffixSet {
    prefix: Bytes,
    pub(super) last: ByteSketch,
    /// The non-empty suffixes end to end, grouped by last byte, each
    /// group in pattern order: suffix `k` is
    /// `suffixes[bounds[k]..bounds[k + 1]]`.
    suffixes: Bytes,
    bounds: Box<[u32]>,
    /// Past `LINEAR_SUFFIXES`: the group of last byte `b` is the suffixes
    /// `groups[b & GROUP_MASK]..groups[(b & GROUP_MASK) + 1]`.
    groups: Box<[u32]>,
    /// One suffix is empty: it ends anything.
    empty: bool,
}

/// A brace-free segment as the pieces its `*`s cut, each of a fixed
/// length. The first is matched at the segment's start, the last at its
/// end, and each between at its leftmost place after the one before,
/// which leaves the most room to the rest.
#[derive(Debug, Clone)]
pub(super) struct Pieces {
    at: Box<[Pos]>,
    /// Piece `k` is `at[ends[k - 1]..ends[k]]`, the first from 0.
    ends: Box<[u32]>,
    classes: Box<[ByteSet]>,
}

/// A byte (folded when case-insensitive), any byte, or a class of
/// `Pieces::classes`.
#[derive(Debug, Clone, Copy)]
enum Pos {
    Byte(u8),
    Any,
    Class(u16),
}

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
    let Some(mut suffixes) = suffix_product(&nodes[idx..]) else {
        return Pieces::wild(nodes, dot, ci).unwrap_or_else(|| SegNfa::wild(nodes, dot, ci));
    };
    let dot_protect = !dot && prefix.is_empty() && (has_star || anychars > 0);
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

/// The literal suffixes after the wildcards, enumerated; `None` when
/// something else follows or the product passes its caps.
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
                let mut literals = Vec::with_capacity(branches.len());
                for b in branches {
                    match b.seq() {
                        [] => literals.push(&b""[..]),
                        [Node::Literal(bytes)] => literals.push(bytes.as_slice()),
                        _ => return None,
                    }
                }
                if parts.len() > 1 && parts.len() * literals.len() > MAX_SUFFIX_PRODUCT {
                    return None;
                }
                let branch_bytes: usize = literals.iter().map(|l| l.len() + 1).sum();
                size = (literals.len().saturating_mul(size - parts.len()))
                    .saturating_add(parts.len().saturating_mul(branch_bytes));
                source += branch_bytes;
                if !fits(size, source) {
                    return None;
                }
                let mut next = Vec::with_capacity(parts.len() * literals.len());
                for p in &parts {
                    for l in &literals {
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

impl SuffixSet {
    fn new(prefix: Bytes, suffixes: Vec<Vec<u8>>, ci: bool) -> Self {
        let last = ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci);
        let empty = suffixes.iter().any(Vec::is_empty);
        let group = |s: &[u8]| (fold(s[s.len() - 1], ci) & GROUP_MASK) as usize;
        let nonempty = || suffixes.iter().filter(|s| !s.is_empty());
        // A counting sort by group keeps the pattern's order within one.
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

    /// Does a suffix of at most `room` bytes (`variable`), or of exactly
    /// `room`, end `seg`? Out of line: inlined, it weighed on every other
    /// wildcard kind.
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
    /// `None` when the segment holds a brace. What the graph reads off the
    /// pieces is what it would read off the NFA.
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
        Some(Wild {
            min_len,
            variable: ends.len() > 1,
            dot_protect: !dot && !can_lit_dot,
            any_seg: false,
            kind: WildKind::Pieces(Box::new(Pieces {
                at: at.into_boxed_slice(),
                ends: ends.into_boxed_slice(),
                classes: classes.into_boxed_slice(),
            })),
        })
    }

    #[inline]
    fn piece(&self, k: usize) -> &[Pos] {
        let from = if k == 0 { 0 } else { self.ends[k - 1] as usize };
        &self.at[from..self.ends[k] as usize]
    }

    /// The last piece first: where names differ most.
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
        for k in 1..last {
            match self.scan(self.piece(k), seg, from, to, ci) {
                Some(end) => from = end,
                None => return false,
            }
        }
        true
    }

    #[inline(always)]
    fn fits(&self, piece: &[Pos], seg: &[u8], ci: bool) -> bool {
        piece
            .iter()
            .zip(seg)
            .rev()
            .all(|(&p, &b)| p.takes(b, &self.classes, ci))
    }

    /// Where the leftmost place of `piece` within `seg[from..to]` ends.
    fn scan(&self, piece: &[Pos], seg: &[u8], from: usize, to: usize, ci: bool) -> Option<usize> {
        let m = piece.len();
        (from..(to + 1).checked_sub(m)?)
            .find(|&k| self.fits(piece, &seg[k..k + m], ci))
            .map(|k| k + m)
    }
}

#[cfg(test)]
mod tests {
    use super::{Pieces, Wild, WildKind};
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

    #[test]
    fn pieces_take_what_the_nfa_does() {
        let tokens = ["a", "B", ".", "?", "*", "[ab]", "[!a]", "[.b]"];
        let names = strings(&["a", "b", ".", "A"], 4);
        for pattern in strings(&tokens, 4).iter().filter(|p| !p.is_empty()) {
            let ast = crate::parse::parse(pattern.as_bytes()).unwrap();
            let nodes = nodes(&ast.body);
            if matches!(nodes[..], [Node::Globstar]) {
                continue;
            }
            // Only `?`s and `*`s are an `Affix`, whose lengths the pieces
            // do not tell.
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
}
