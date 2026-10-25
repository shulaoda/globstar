//! In-segment wildcard matchers: what a segment with a wildcard compiles
//! to (`compile_wild`) and how it takes a path segment (`wild_consumes`);
//! the in-segment NFA and the glue NFA.

use std::collections::HashMap;

use crate::bytes::{affix_eq, fold};
use crate::parse::ast::Node;

use super::bits::{Bits, ByteSet, ByteSketch, Wide};
use super::graph::{ACCEPT, Edges, Elem, INNER};
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

// The in-segment NFA (`WildKind::Generic`): a Thompson NFA over the
// segment's nodes, simulated with a bit set. Every ε-edge points to a
// later state, so one ascending sweep closes a set. The glue NFA of the
// pieces a brace cuts is built of the same states.

const UNSET: u32 = u32::MAX;

/// The most the per-state closures may take (about 1,000 states); past
/// it a step closes its set as it goes, which keeps memory linear.
const MAX_CLOSURE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub(super) enum SegNfa {
    Small(SegNfaImpl<u64>),
    Wide(SegNfaImpl<Wide>),
}

#[derive(Debug, Clone)]
pub(super) struct SegNfaImpl<B: Bits> {
    states: Box<[SegState]>,
    init: B,
    /// The entry closure of a protected leading `.`: `*`, `?` and negated
    /// classes dropped, no `*` passed.
    init_dot_blocked: B,
    /// Per-state ε-closures; empty past `MAX_CLOSURE_BYTES`.
    closures: Box<[B]>,
    accept: usize,
    words: usize,
}

#[derive(Debug, Clone)]
enum SegState {
    Byte(u8, u32),
    Class(Box<ByteSet>, u32),
    /// Takes any byte. A `?` moves to `.0`; a `*` is its own `.0` and
    /// leaves by the ε-edge `.1`. One kind keeps the hot loop at three arms.
    Any(u32, u32),
    Split(u32, u32),
    Jump(u32),
    Match,
    /// A glued piece's segment may end here, at graph node `.0`.
    Exit(u32),
}

/// The lengths of the segments the wildcards alone take to a state: one
/// bit each below 64, and every length from `tail` on (`u32::MAX`: none).
/// Lengths past 63 below the tail are left out, which keeps out lengths
/// only.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lengths {
    low: u64,
    tail: u32,
}

/// The pieces of the segments a brace cuts, as one NFA: a piece's states,
/// then a fan of ε-edges to the pieces glued after it and an `Exit` where
/// its segment may end. A piece is glued only to later ones, so every
/// ε-edge still points to a later state.
#[derive(Debug, Clone)]
pub(super) struct GlueNfa {
    states: Box<[SegState]>,
    closures: Box<[Wide]>,
    words: usize,
    /// Per graph node, the range of its states.
    spans: Box<[(u32, u32)]>,
}

struct SegBuilder {
    states: Vec<SegState>,
    tails: Vec<u32>,
    ci: bool,
}

impl SegNfa {
    pub(super) fn wild(nodes: &[&Node], dot: bool, ci: bool) -> Wild {
        let mut builder = SegBuilder {
            states: Vec::with_capacity(16),
            tails: Vec::new(),
            ci,
        };
        let entry = builder.compile_nodes(nodes.iter().copied()) as usize;
        let accept = builder.alloc(SegState::Match);
        for t in std::mem::take(&mut builder.tails) {
            builder.patch(t, accept);
        }
        let states = builder.states.into_boxed_slice();
        if states.len() <= 64 {
            Self::build(states, entry, dot, SegNfa::Small)
        } else {
            Self::build(states, entry, dot, SegNfa::Wide)
        }
    }

    fn build<B: Bits>(
        states: Box<[SegState]>,
        entry: usize,
        dot: bool,
        kind: fn(SegNfaImpl<B>) -> SegNfa,
    ) -> Wild {
        let n = states.len();
        let words = B::words(n);
        let closures = if n * words * 8 <= MAX_CLOSURE_BYTES {
            closures(&states, words)
        } else {
            Box::default()
        };
        let entry_set = |block| {
            let mut set = B::zero(words);
            set.set(entry);
            close(&states, &mut set, block);
            set
        };
        let init = entry_set(false);
        let init_dot_blocked = if dot { init.clone() } else { entry_set(true) };
        // The segment is dot-protected when nothing can take a protected
        // leading `.` by name.
        let mut can_lit_dot = false;
        init_dot_blocked.for_each(|s| match &states[s] {
            SegState::Byte(x, _) => can_lit_dot |= *x == b'.',
            SegState::Class(cls, _) => can_lit_dot |= cls.has(b'.'),
            _ => {}
        });
        let satisfiable = satisfiable::<B>(&states, entry, words);
        let nfa = SegNfaImpl {
            states,
            init,
            init_dot_blocked,
            closures,
            accept: n - 1,
            words,
        };
        Wild {
            min_len: if satisfiable { 0 } else { NEVER },
            variable: true,
            dot_protect: !dot && !can_lit_dot,
            any_seg: nfa.any_seg(),
            kind: WildKind::Generic(Box::new(kind(nfa))),
        }
    }

    pub(super) fn matches(&self, seg: &[u8]) -> bool {
        match self {
            SegNfa::Small(n) => n.matches(seg),
            SegNfa::Wide(n) => n.matches(seg),
        }
    }
}

impl<B: Bits> SegNfaImpl<B> {
    #[inline]
    fn matches(&self, seg: &[u8]) -> bool {
        let active = if seg.first() == Some(&b'.') {
            self.init_dot_blocked.clone()
        } else {
            self.init.clone()
        };
        run(&self.states, &self.closures, active, seg, self.words).test(self.accept)
    }

    /// Do the wildcards alone take every non-empty segment?
    fn any_seg(&self) -> bool {
        let mut any = false;
        self.init
            .for_each(|s| any |= matches!(self.states[s], SegState::Any(..)));
        any && wild_lengths(&self.states, &self.init)[self.accept].all_nonempty()
    }
}

impl Lengths {
    const NONE: Lengths = Lengths {
        low: 0,
        tail: u32::MAX,
    };

    fn add(&mut self, other: Lengths) {
        self.low |= other.low;
        self.tail = self.tail.min(other.tail);
    }

    /// One wildcard more.
    fn shift(self) -> Lengths {
        Lengths {
            low: self.low << 1,
            tail: self.tail.saturating_add(1),
        }
    }

    /// Every length from the least on.
    fn star(self) -> Lengths {
        let least = if self.low == 0 {
            self.tail
        } else {
            self.low.trailing_zeros()
        };
        Lengths {
            low: self.low,
            tail: self.tail.min(least),
        }
    }

    fn all_nonempty(self) -> bool {
        let run = if self.tail < 64 {
            u64::MAX << self.tail
        } else {
            0
        };
        self.tail <= 64 && (self.low | run | 1) == u64::MAX
    }
}

/// The lengths from the states in `entry` to each state, in one pass in
/// state order: every edge but a star's loop points to a later state.
fn wild_lengths<B: Bits>(states: &[SegState], entry: &B) -> Vec<Lengths> {
    let mut at = vec![Lengths::NONE; states.len()];
    entry.for_each(|s| at[s].low |= 1);
    for s in 0..states.len() {
        let mut here = at[s];
        if here == Lengths::NONE {
            continue;
        }
        let (a, b, by) = match states[s] {
            SegState::Split(a, b) => (a, b, here),
            SegState::Jump(t) => (t, UNSET, here),
            SegState::Any(next, exit) if next as usize == s => {
                here = here.star();
                at[s] = here;
                (exit, UNSET, here)
            }
            SegState::Any(next, _) => (next, UNSET, here.shift()),
            _ => continue,
        };
        for t in [a, b] {
            if t != UNSET {
                at[t as usize].add(by);
            }
        }
    }
    at
}

/// Can the entry reach `Match` through consumers some byte fires?
fn satisfiable<B: Bits>(states: &[SegState], entry: usize, words: usize) -> bool {
    let mut reach = B::zero(words);
    reach.set(entry);
    for (s, state) in states.iter().enumerate().skip(entry) {
        if !reach.test(s) {
            continue;
        }
        let (a, b) = match state {
            SegState::Match | SegState::Exit(_) => return true,
            SegState::Class(cls, _) if cls.is_empty() => continue,
            SegState::Split(a, b) | SegState::Any(a, b) => (*a, *b),
            SegState::Byte(_, n) | SegState::Class(_, n) | SegState::Jump(n) => (*n, UNSET),
        };
        for t in [a, b] {
            if t != UNSET {
                reach.set(t as usize);
            }
        }
    }
    false
}

/// Close `set` under ε-edges, keeping consumers, `Match` and `Exit`. The
/// word being swept stays in a register, where most edges land. `block`:
/// the entry set of a protected leading `.`.
fn close<B: Bits>(states: &[SegState], set: &mut B, block: bool) {
    let words = set.words_mut();
    for w in 0..words.len() {
        let mut word = words[w];
        let mut todo = word;
        while todo != 0 {
            let bit = todo.isolate_lowest_one();
            todo ^= bit;
            let s = w * 64 + bit.trailing_zeros() as usize;
            let (a, b, stays) = match &states[s] {
                SegState::Split(a, b) => (*a, *b, false),
                SegState::Jump(t) => (*t, UNSET, false),
                SegState::Any(..) if block => (UNSET, UNSET, false),
                SegState::Any(_, exit) if *exit != UNSET => (*exit, UNSET, true),
                SegState::Class(cls, _) if block && cls.negated => (UNSET, UNSET, false),
                SegState::Byte(..)
                | SegState::Class(..)
                | SegState::Any(..)
                | SegState::Match
                | SegState::Exit(_) => {
                    continue;
                }
            };
            if !stays {
                word ^= bit;
            }
            for t in [a, b] {
                if t == UNSET {
                    continue;
                }
                let tb = 1u64 << (t & 63);
                if t as usize >> 6 == w {
                    word |= tb;
                    todo |= tb;
                } else {
                    words[t as usize >> 6] |= tb;
                }
            }
        }
        words[w] = word;
    }
}

/// `active` after every byte of `seg`: through the kept closures, or
/// closing each step's set when there are none. Forced inline: it is the
/// in-segment NFAs' whole inner loop.
#[inline(always)]
fn run<B: Bits>(states: &[SegState], closures: &[B], mut active: B, seg: &[u8], words: usize) -> B {
    let mut next = B::zero(words);
    if closures.is_empty() {
        for &c in seg {
            if active.is_zero() {
                break;
            }
            next.clear();
            active.for_each(|s| match &states[s] {
                SegState::Byte(b, nx) if *b == c => next.set(*nx as usize),
                SegState::Class(cls, nx) if cls.has(c) => next.set(*nx as usize),
                SegState::Any(nx, _) => next.set(*nx as usize),
                _ => {}
            });
            close(states, &mut next, false);
            std::mem::swap(&mut active, &mut next);
        }
        return active;
    }
    for &c in seg {
        if active.is_zero() {
            break;
        }
        next.clear();
        active.for_each(|s| match &states[s] {
            SegState::Byte(b, nx) if *b == c => next.or_with(&closures[*nx as usize]),
            SegState::Class(cls, nx) if cls.has(c) => next.or_with(&closures[*nx as usize]),
            SegState::Any(nx, _) => next.or_with(&closures[*nx as usize]),
            _ => {}
        });
        std::mem::swap(&mut active, &mut next);
    }
    active
}

/// Each state's ε-closure, in one backward pass.
fn closures<B: Bits>(states: &[SegState], words: usize) -> Box<[B]> {
    let n = states.len();
    // State `t` sits at `n - 1 - t` until the final reverse.
    let mut out: Vec<B> = Vec::with_capacity(n);
    for s in (0..n).rev() {
        let done = |t: u32| &out[n - 1 - t as usize];
        let c = match &states[s] {
            SegState::Split(a, b) => {
                let mut c = done(*a).clone();
                c.or_with(done(*b));
                c
            }
            SegState::Jump(t) => done(*t).clone(),
            SegState::Any(_, exit) if *exit != UNSET => {
                let mut c = done(*exit).clone();
                c.set(s);
                c
            }
            _ => {
                let mut c = B::zero(words);
                c.set(s);
                c
            }
        };
        out.push(c);
    }
    out.reverse();
    out.into_boxed_slice()
}

impl GlueNfa {
    /// Of the nodes glued to their predecessors or with a successor glued
    /// to them, each of which becomes an `Elem::Piece`.
    pub(super) fn new(
        elems: &mut [Elem],
        edges: &Edges,
        glued: &[bool],
        pieces: &[Vec<&Node>],
        ci: bool,
    ) -> Self {
        let n = elems.len();
        let glued_to = |t: u32| t != ACCEPT && glued[t as usize];
        let mut builder = SegBuilder {
            states: Vec::new(),
            tails: Vec::new(),
            ci,
        };
        let mut spans = vec![(0, 0); n];
        // The fan states that wait for a later piece's first state.
        let mut waiting: Vec<Vec<u32>> = vec![Vec::new(); n];
        for i in 0..n {
            let succ = edges.succ(i);
            if !glued[i] && !succ.iter().any(|&t| glued_to(t)) {
                continue;
            }
            let first = builder.states.len() as u32;
            let (lit, one);
            let nodes: &[&Node] = match &elems[i] {
                Elem::Lit(bytes) => {
                    lit = Node::Literal(bytes.to_vec());
                    one = [&lit];
                    &one
                }
                Elem::Wild(_) => &pieces[i],
                Elem::Join => &[],
                _ => unreachable!("a globstar is never glued"),
            };
            let entry = builder.compile_nodes(nodes.iter().copied());
            for s in std::mem::take(&mut waiting[i]) {
                builder.patch(s, entry);
            }
            let tails = std::mem::take(&mut builder.tails);
            let exit = succ.iter().any(|&t| !glued_to(t));
            let glues = succ.iter().filter(|&&t| glued_to(t));
            let ways = usize::from(exit) + glues.clone().count();
            // Split `k` takes leaf `k` or the rest, as a brace's do.
            let fan = builder.states.len();
            for _ in 1..ways {
                builder.alloc(SegState::Split(UNSET, UNSET));
            }
            let mut leaves = Vec::with_capacity(ways);
            if exit {
                leaves.push(builder.alloc(SegState::Exit(i as u32)));
            }
            for &t in glues {
                let s = builder.alloc(SegState::Jump(UNSET));
                waiting[t as usize].push(s);
                leaves.push(s);
            }
            for k in 0..ways - 1 {
                let rest = if k + 2 < ways {
                    (fan + k + 1) as u32
                } else {
                    leaves[k + 1]
                };
                builder.states[fan + k] = SegState::Split(leaves[k], rest);
            }
            let fan_entry = if ways > 1 { fan as u32 } else { leaves[0] };
            for t in tails {
                builder.patch(t, fan_entry);
            }
            spans[i] = (first, builder.states.len() as u32);
            elems[i] = Elem::Piece(if glued[i] { INNER } else { entry });
        }
        let states = builder.states.into_boxed_slice();
        let words = Wide::words(states.len());
        let closures = if states.len() * words * 8 <= MAX_CLOSURE_BYTES {
            closures(&states, words)
        } else {
            Box::default()
        };
        GlueNfa {
            states,
            closures,
            words,
            spans: spans.into_boxed_slice(),
        }
    }

    pub(super) fn set(&self) -> Wide {
        Wide::zero(self.words)
    }

    /// Calls `exit` with each node whose piece ends a match of `seg` from
    /// the entry states in `active`.
    pub(super) fn exits(
        &self,
        mut active: Wide,
        seg: &[u8],
        dot: bool,
        mut exit: impl FnMut(usize),
    ) {
        close(
            &self.states,
            &mut active,
            !dot && seg.first() == Some(&b'.'),
        );
        run(&self.states, &self.closures, active, seg, self.words).for_each(|s| {
            if let SegState::Exit(node) = self.states[s] {
                exit(node as usize);
            }
        });
    }

    /// Calls `exit` with each node at which the wildcards alone, from the
    /// entry states in `entries`, end every non-empty segment.
    pub(super) fn any_exits(&self, entries: &Wide, mut exit: impl FnMut(usize)) {
        let at = wild_lengths(&self.states, entries);
        for (state, lengths) in self.states.iter().zip(at) {
            if let SegState::Exit(node) = state
                && lengths.all_nonempty()
            {
                exit(*node as usize);
            }
        }
    }

    /// Adds to `good` the states of `node` from which some segment
    /// reaches an exit in `good` (`node`'s own when `exit_on`); whether
    /// its entry is one. Called from the last node back.
    pub(super) fn reach(&self, node: usize, exit_on: bool, good: &mut Wide) -> bool {
        let (lo, hi) = self.spans[node];
        for s in (lo as usize..hi as usize).rev() {
            let to = |t: u32| t != UNSET && t as usize != s && good.test(t as usize);
            let on = match &self.states[s] {
                SegState::Exit(_) => exit_on,
                SegState::Class(cls, _) if cls.is_empty() => false,
                SegState::Byte(_, n) | SegState::Class(_, n) | SegState::Jump(n) => to(*n),
                SegState::Any(a, b) | SegState::Split(a, b) => to(*a) || to(*b),
                SegState::Match => false,
            };
            if on {
                good.set(s);
            }
        }
        lo < hi && good.test(lo as usize)
    }
}

impl SegBuilder {
    fn alloc(&mut self, s: SegState) -> u32 {
        self.states.push(s);
        (self.states.len() - 1) as u32
    }

    /// Point the dangling edge of `state` at `target`: a `*`'s exit, or
    /// the `next` of anything else.
    fn patch(&mut self, state: u32, target: u32) {
        match &mut self.states[state as usize] {
            SegState::Any(n, exit) if *n == state => *exit = target,
            SegState::Byte(_, n)
            | SegState::Class(_, n)
            | SegState::Any(n, _)
            | SegState::Jump(n) => *n = target,
            SegState::Split(..) | SegState::Match | SegState::Exit(_) => unreachable!(),
        }
    }

    fn compile_nodes<'a>(&mut self, nodes: impl IntoIterator<Item = &'a Node>) -> u32 {
        let mut entry: Option<u32> = None;
        let mut pending: Vec<u32> = Vec::new();
        for node in nodes {
            let (node_entry, mut node_tails) = self.compile_node(node);
            for t in pending.drain(..) {
                self.patch(t, node_entry);
            }
            pending.append(&mut node_tails);
            if entry.is_none() {
                entry = Some(node_entry);
            }
        }
        self.tails.append(&mut pending);
        entry.unwrap_or_else(|| {
            // An empty branch: a jump that waits for what follows.
            let s = self.alloc(SegState::Jump(UNSET));
            self.tails.push(s);
            s
        })
    }

    fn compile_node(&mut self, node: &Node) -> (u32, Vec<u32>) {
        match node {
            Node::Literal(bytes) => {
                debug_assert!(!bytes.is_empty());
                let entry = self.lit_state(bytes[0]);
                let mut prev = entry;
                for &b in &bytes[1..] {
                    let s = self.lit_state(b);
                    self.patch(prev, s);
                    prev = s;
                }
                (entry, vec![prev])
            }
            Node::AnyChar => {
                let s = self.alloc(SegState::Any(UNSET, UNSET));
                (s, vec![s])
            }
            Node::Class(cls) => {
                let s = self.alloc(SegState::Class(Box::new(ByteSet::of(cls, self.ci)), UNSET));
                (s, vec![s])
            }
            Node::Star => {
                let s = self.states.len() as u32;
                self.alloc(SegState::Any(s, UNSET));
                (s, vec![s])
            }
            Node::Brace(branches) => {
                debug_assert!(!branches.is_empty());
                // The splits come before the branches, so every ε-edge
                // points to a later state.
                let first = self.states.len();
                for _ in 1..branches.len() {
                    self.alloc(SegState::Split(UNSET, UNSET));
                }
                let mut entries = Vec::with_capacity(branches.len());
                let mut tails: Vec<u32> = Vec::new();
                for branch in branches {
                    entries.push(self.compile_nodes(branch.seq().iter()));
                    tails.append(&mut self.tails);
                }
                // Split `i` picks branch `i` or the rest.
                let last = entries.len() - 1;
                for i in 0..last {
                    let rest = if i + 1 < last {
                        (first + i + 1) as u32
                    } else {
                        entries[last]
                    };
                    self.states[first + i] = SegState::Split(entries[i], rest);
                }
                (if last == 0 { entries[0] } else { first as u32 }, tails)
            }
            _ => unreachable!("separator-crossing node inside a segment"),
        }
    }

    fn lit_state(&mut self, b: u8) -> u32 {
        if !(self.ci && b.is_ascii_alphabetic()) {
            return self.alloc(SegState::Byte(b, UNSET));
        }
        let mut set = ByteSet {
            bits: [0; 4],
            negated: false,
        };
        set.add(b.to_ascii_lowercase());
        set.add(b.to_ascii_uppercase());
        self.alloc(SegState::Class(Box::new(set), UNSET))
    }
}

#[cfg(test)]
mod tests {
    use super::SegNfa;
    use super::{Pieces, Wild, WildKind};
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

    #[test]
    fn any_seg_is_found_in_one_pass() {
        let any_seg = |pattern: &str| {
            let ast = crate::parse::parse(pattern.as_bytes()).unwrap();
            let nodes: Vec<&Node> = match &ast.body {
                Node::Concat(nodes) => nodes.iter().collect(),
                node => vec![node],
            };
            SegNfa::wild(&nodes, true, false).any_seg
        };
        let run = |unit: &str, n: usize| unit.repeat(n);
        assert!(!any_seg(&run("{?,}", 100)));
        assert!(any_seg(&(run("{,?}", 100) + "*")));
        assert!(!any_seg("??*"));
        assert!(any_seg("{?,??*}"));
        assert!(!any_seg("{?,???*}"));
        assert!(!any_seg("{?,??*}a"));
        // A run a star's lengths join below 64 goes on for good; one they
        // join past 63 is not told.
        let joined = |stars: usize| format!("{{{},{}*}}", run("{?,}", 80), run("?", stars));
        assert!(any_seg(&joined(60)));
        assert!(!any_seg(&joined(70)));
    }
}
