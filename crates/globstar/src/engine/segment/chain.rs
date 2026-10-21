//! The chains: every path through the graph, matched by anchored
//! compares when it has at most one globstar or two around a block,
//! the literal-only paths as one sorted set, and `match_dir` read off
//! a lone chain.
//!
//! The predicates that run once per segment or per element
//! (`consume_at`, `elem_consumes`, `absorbs`, `block_from`) and the
//! tail walk the line and block shapes share (`tail_split`) are forced
//! inline: LLVM leaves them out of line on its own, and a call there
//! costs 10–40% on the shortest paths. Everything else is left to the
//! compiler.

use std::cmp::Ordering;

use crate::dir_match::DirMatch;

use super::graph::{ACCEPT, Graph};
use super::wild::{ByteSketch, WildKind, fold};
use super::{Elem, SegmentMatcher, seg_end, seg_start};

// ── the chains ──────────────────────────────────────────────────────────

/// Paths with a wildcard `is_match` tries one by one; past that the
/// NFA over the whole graph runs.
const MAX_CHAINS: usize = 64;

/// The literal-only paths kept as one set (a union of file names), and
/// the bytes it may take, or four times the pattern's weight (a long
/// literal behind crossing braces is copied once per path); past
/// either the NFA over the whole graph runs.
const MAX_LITERALS: usize = 4096;
const MAX_LITERAL_BYTES: usize = 64 * 1024;

/// The element compares `chain_dir` spends on tail tokens; past them it
/// answers what is safe ("may match below", "not all below"). Tails of
/// up to 90 elements stay exact.
const MAX_TAIL_CHECKS: usize = 4096;

/// One path through the graph.
#[derive(Debug, Clone)]
pub(super) struct Chain {
    /// Its nodes in order; empty for the lone path of a pattern
    /// without a separator-crossing brace, whose nodes are
    /// `0..elems.len()`, and for the set of literal paths.
    pub(super) nodes: Box<[u32]>,
    pub(super) shape: Shape,
}

/// Where a chain's globstars are, which decides how it is matched.
#[derive(Debug, Clone)]
pub(super) enum Shape {
    /// At most one globstar: the head and the tail are anchored around
    /// it.
    Line(Line),
    /// Two globstars around a block of elements (`**/x/**`): the head
    /// and the tail are anchored, the block floats between them.
    Block { g1: u32, g2: u32 },
    /// The literal paths, all of them: one search answers them.
    Set(Box<Literals>),
}

/// A chain with at most one globstar, at `g` (past the end for none).
/// Every element from `open_from` on takes some segment, from
/// `any_from` on any non-empty one: what `match_dir` reads off a lone
/// chain (both 0 on a union member's, which `match_dir` never reads).
#[derive(Debug, Clone, Copy)]
pub(super) struct Line {
    g: u32,
    open_from: u32,
    any_from: u32,
}

/// The literal paths through the graph: the bytes each matches, sorted
/// as `cmp_lit_path` orders them against a path, laid end to end. A
/// union of file names is answered by one search of them.
#[derive(Debug, Clone)]
pub(super) struct Literals {
    /// Path `k` is `bytes[bounds[k]..bounds[k + 1]]`.
    bytes: Box<[u8]>,
    bounds: Box<[u32]>,
}

impl Literals {
    /// From the paths laid end to end in `raw`, path `k` being
    /// `raw[raw_bounds[k]..raw_bounds[k + 1]]`: sorted, without repeats.
    fn new(raw: &[u8], raw_bounds: &[u32], ci: bool) -> Self {
        let at = |k: usize| &raw[raw_bounds[k] as usize..raw_bounds[k + 1] as usize];
        let mut order: Vec<usize> = (0..raw_bounds.len() - 1).collect();
        order.sort_unstable_by(|&a, &b| cmp_lit_path(at(a), at(b), ci));
        order.dedup_by(|&mut a, &mut b| cmp_lit_path(at(a), at(b), ci) == Ordering::Equal);
        let mut bytes = Vec::with_capacity(raw.len());
        let mut bounds = Vec::with_capacity(order.len() + 1);
        bounds.push(0);
        for k in order {
            bytes.extend_from_slice(at(k));
            bounds.push(bytes.len() as u32);
        }
        Literals {
            bytes: bytes.into_boxed_slice(),
            bounds: bounds.into_boxed_slice(),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.bounds.len() - 1
    }

    /// Is the path one of them?
    pub(super) fn contains(&self, path: &[u8], ci: bool) -> bool {
        let (mut lo, mut hi) = (0usize, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            // One bounds check for the pair: 2% of a set row's call.
            let [start, end] = self.bounds[mid..mid + 2] else {
                unreachable!("mid < len");
            };
            let lit = &self.bytes[start as usize..end as usize];
            match cmp_lit_path(lit, path, ci) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return true,
            }
        }
        false
    }
}

/// A literal path against a path: bytes as they are, unless
/// case-insensitive (ASCII folded) or on Windows (a path's separators
/// as `/`).
fn cmp_lit_path(lit: &[u8], path: &[u8], ci: bool) -> Ordering {
    if !ci && !cfg!(windows) {
        return lit.cmp(path);
    }
    lit.iter().map(|&b| fold(b, ci)).cmp(path.iter().map(|&b| {
        if std::path::is_separator(char::from(b)) {
            b'/'
        } else {
            fold(b, ci)
        }
    }))
}

/// A union's cheap look at a path before a chain runs, read off the
/// chain's ends: the bytes a matching path may end with (`ALL` when
/// the last element ends with anything), and the byte it must start
/// with when the first element is a literal, folded when
/// case-insensitive. `NO_HEAD` says nothing about the start; a literal
/// starting with that byte reads the same, and the walk checks it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Gate {
    last: ByteSketch,
    head: u8,
}

/// `Gate::head` when the path's first byte says nothing.
const NO_HEAD: u8 = 0;

// ── building ────────────────────────────────────────────────────────────

impl Graph {
    /// Every path through the graph as a `Chain`, in pattern order, the
    /// literal ones together as one `Shape::Set` chain first; none when
    /// the others pass `MAX_CHAINS`, the literal ones `MAX_LITERALS` or
    /// their bytes, or a path has more than two globstars (the NFA
    /// takes those).
    pub(super) fn chains(&self, ci: bool) -> Box<[Chain]> {
        let Some(edges) = &self.edges else {
            return self.chain_of(None, true).into_iter().collect();
        };
        let mut paths: Vec<Vec<u32>> = Vec::new();
        // The literal paths' bytes, end to end, and where each starts
        // and the last ends.
        let mut lits: Vec<u8> = Vec::new();
        let mut lit_bounds: Vec<u32> = vec![0];
        // Depth-first, first branch first, along one path: a level per
        // node on it (the start's below them) and the successor it tries
        // next. A path is copied only when it reaches the end.
        let mut path: Vec<u32> = Vec::new();
        let mut levels: Vec<(&[u32], usize)> = vec![(edges.succ(self.elems.len()), 0)];
        while let Some((succ, next)) = levels.last_mut() {
            let Some(&id) = succ.get(*next) else {
                levels.pop();
                path.pop();
                continue;
            };
            *next += 1;
            if id == ACCEPT {
                let literal = path
                    .iter()
                    .all(|&i| matches!(self.elems[i as usize], Elem::Lit(_)));
                if literal {
                    self.literal_path(&path, &mut lits);
                    lit_bounds.push(lits.len() as u32);
                } else {
                    paths.push(path.clone());
                }
                if paths.len() > MAX_CHAINS
                    || lit_bounds.len() > MAX_LITERALS + 1
                    || lits.len() > MAX_LITERAL_BYTES.max(4 * self.weight)
                {
                    return Box::default();
                }
                continue;
            }
            path.push(id);
            levels.push((edges.succ(id as usize), 0));
        }
        let lone = paths.len() == 1 && lit_bounds.len() == 1;
        let set = (lit_bounds.len() > 1).then(|| Chain {
            nodes: Box::default(),
            shape: Shape::Set(Box::new(Literals::new(&lits, &lit_bounds, ci))),
        });
        set.into_iter()
            .map(Some)
            .chain(paths.into_iter().map(|p| self.chain_of(Some(p), lone)))
            .collect::<Option<_>>()
            .unwrap_or_default()
    }

    /// The bytes a path of literal nodes matches, the segments joined by
    /// `/`, appended to `out`.
    fn literal_path(&self, path: &[u32], out: &mut Vec<u8>) {
        for (k, &i) in path.iter().enumerate() {
            if k > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(self.elems[i as usize].as_lit());
        }
    }

    /// The chain of a path (`path` `None`: the lone path of a linear
    /// pattern, whose nodes are `0..elems.len()`); none when it has more
    /// than two globstars, or two with nothing between (the graph's
    /// NFA serves).
    fn chain_of(&self, path: Option<Vec<u32>>, lone: bool) -> Option<Chain> {
        let n = path.as_ref().map_or(self.elems.len(), Vec::len);
        let elem = |i: usize| &self.elems[path.as_ref().map_or(i, |p| p[i] as usize)];
        let mut globstars = (0..n).filter(|&i| elem(i).is_globstar());
        // The first index from which every element passes `ok`.
        let from = |ok: fn(&Elem) -> bool| {
            (0..n).rev().find(|&j| !ok(elem(j))).map_or(0, |j| j + 1) as u32
        };
        let shape = match (globstars.next(), globstars.next(), globstars.next()) {
            (Some(g1), Some(g2), None) if g2 > g1 + 1 => Shape::Block {
                g1: g1 as u32,
                g2: g2 as u32,
            },
            (Some(_), Some(_), _) => return None,
            // `match_dir` reads `open_from` and `any_from` off the lone
            // chain only.
            (g, ..) => Shape::Line(Line {
                g: g.unwrap_or(n) as u32,
                open_from: if lone { from(Elem::satisfiable) } else { 0 },
                any_from: if lone { from(Elem::any_seg) } else { 0 },
            }),
        };
        Some(Chain {
            nodes: path.map_or_else(Default::default, Vec::into_boxed_slice),
            shape,
        })
    }
}

impl Chain {
    pub(super) fn gate(&self, elems: &[Elem], ci: bool) -> Gate {
        // The set of literal paths is searched before any gate.
        if matches!(self.shape, Shape::Set(_)) {
            return Gate {
                last: ByteSketch::ALL,
                head: NO_HEAD,
            };
        }
        let first = self.nodes.first().map_or(0, |&n| n as usize);
        let last = self.nodes.last().map_or(elems.len() - 1, |&n| n as usize);
        let last = match &elems[last] {
            Elem::Lit(lit) => ByteSketch::of_last_bytes(std::iter::once(&lit[..]), ci),
            Elem::Wild(w) => match &w.kind {
                WildKind::Affix(affix) => {
                    ByteSketch::of_last_bytes(std::iter::once(affix.parts().1), ci)
                }
                WildKind::Suffixes(set) => set.last,
                WildKind::Generic(_) => ByteSketch::ALL,
            },
            _ => ByteSketch::ALL,
        };
        let head = match &elems[first] {
            Elem::Lit(lit) => lit.first().map_or(NO_HEAD, |&b| fold(b, ci)),
            _ => NO_HEAD,
        };
        Gate { last, head }
    }
}

// ── matching ────────────────────────────────────────────────────────────

/// A chain's elements in order: the matcher's slice for the lone path
/// of a linear pattern, a node list over it otherwise.
trait ChainElems {
    fn len(&self) -> usize;
    fn at(&self, i: usize) -> &Elem;
}

impl ChainElems for &[Elem] {
    #[inline]
    fn len(&self) -> usize {
        <[Elem]>::len(self)
    }
    #[inline]
    fn at(&self, i: usize) -> &Elem {
        &self[i]
    }
}

struct Mapped<'a> {
    nodes: &'a [u32],
    elems: &'a [Elem],
}

impl ChainElems for Mapped<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.nodes.len()
    }
    #[inline]
    fn at(&self, i: usize) -> &Elem {
        &self.elems[self.nodes[i] as usize]
    }
}

impl SegmentMatcher {
    /// Does one of the chains match the whole path? The literal paths
    /// by their search; each chain after its gates: the path's last
    /// byte against the chain's sketch, its first against a literal
    /// head's, then the chain's whole last element. Out of line:
    /// `is_match` stays small for the one-chain and NFA matchers.
    #[inline(never)]
    pub(super) fn any_chain_matches(&self, gates: &[Gate], path: &[u8]) -> bool {
        // The literal paths' set, when there is one, comes first.
        let from = match &self.chains[0].shape {
            Shape::Set(set) => {
                if set.contains(path, self.case_insensitive) {
                    return true;
                }
                1
            }
            _ => 0,
        };
        let first = path
            .first()
            .map_or(NO_HEAD, |&b| fold(b, self.case_insensitive));
        // Where the last segment starts, found once some chain asks.
        let mut last_start = None;
        self.chains[from..]
            .iter()
            .zip(&gates[from..])
            .any(|(chain, gate)| {
                gate.last.may_end(path)
                    && (gate.head == NO_HEAD || gate.head == first)
                    && self.ends_like(chain, path, &mut last_start)
                    && self.chain_matches(chain, path)
            })
    }

    /// Does the path end like the chain's last element: a literal, an
    /// affix's suffix (or, without one, its prefix at the last segment's
    /// start)? What the sketch let through is told apart here, before
    /// the chain runs.
    #[inline]
    fn ends_like(&self, chain: &Chain, path: &[u8], last_start: &mut Option<usize>) -> bool {
        let last = chain
            .nodes
            .last()
            .map_or(self.elems.len() - 1, |&n| n as usize);
        let kind = match &self.elems[last] {
            Elem::Lit(lit) => return self.tail_is(path, lit),
            Elem::Wild(w) => &w.kind,
            _ => return true,
        };
        match kind {
            WildKind::Affix(affix) => match affix.parts() {
                (_, suffix) if !suffix.is_empty() => self.tail_is(path, suffix),
                (prefix, _) => {
                    let start = *last_start.get_or_insert_with(|| seg_start(path, path.len()));
                    path.len() - start >= prefix.len()
                        && self.affix_eq(prefix, &path[start..start + prefix.len()])
                }
            },
            WildKind::Suffixes(_) | WildKind::Generic(_) => true,
        }
    }

    #[inline]
    fn tail_is(&self, path: &[u8], suffix: &[u8]) -> bool {
        path.len() >= suffix.len() && self.affix_eq(suffix, &path[path.len() - suffix.len()..])
    }

    /// Does the chain match the whole path?
    pub(super) fn chain_matches(&self, chain: &Chain, path: &[u8]) -> bool {
        if chain.nodes.is_empty() {
            self.chain_matches_on(chain, &&self.elems[..], path)
        } else {
            let elems = Mapped {
                nodes: &chain.nodes,
                elems: &self.elems,
            };
            self.chain_matches_on(chain, &elems, path)
        }
    }

    #[inline]
    fn chain_matches_on<E: ChainElems>(&self, chain: &Chain, elems: &E, path: &[u8]) -> bool {
        match &chain.shape {
            Shape::Line(line) if line.g as usize == elems.len() => {
                self.fixed_run(elems, 0..elems.len(), path, 0)
            }
            Shape::Line(line) => {
                let g = line.g as usize;
                self.head(elems, g, path)
                    .is_some_and(|mid_start| self.tail_and_globstar(elems, g, path, mid_start))
            }
            Shape::Block { g1, g2 } => {
                let (g1, g2) = (*g1 as usize, *g2 as usize);
                let Some(mid_start) = self.head(elems, g1, path) else {
                    return false;
                };
                let Some(split) = self.tail_split(elems, g2, path, mid_start) else {
                    return false;
                };
                self.block_between(elems, g1, g2, path, mid_start, split)
            }
            Shape::Set(_) => unreachable!("the set is searched by any_chain_matches"),
        }
    }

    /// `elems[i]` on the segment at `pos`, the path's last one iff
    /// `last`: `Some(end)` is the index just past it. The segment is
    /// bounded first, so a path of the wrong depth is rejected before
    /// any wildcard runs; a literal is then compared in place.
    #[inline(always)]
    fn consume_at<E: ChainElems>(
        &self,
        elems: &E,
        i: usize,
        path: &[u8],
        pos: usize,
        last: bool,
    ) -> Option<usize> {
        let (end, taken) = match elems.at(i) {
            Elem::Lit(lit) => {
                let end = pos + lit.len();
                let bounded = if last {
                    end == path.len()
                } else {
                    end < path.len() && std::path::is_separator(path[end] as char)
                };
                (end, bounded && self.affix_eq(lit, &path[pos..end]))
            }
            Elem::Wild(w) => {
                let end = seg_end(path, pos);
                let bounded = (end == path.len()) == last;
                (end, bounded && self.wild_consumes(w, &path[pos..end]))
            }
            _ => return None,
        };
        taken.then_some(end)
    }

    /// Do `elems[range]` take the segments from `pos` to the end, one
    /// each?
    fn fixed_run<E: ChainElems>(
        &self,
        elems: &E,
        range: std::ops::Range<usize>,
        path: &[u8],
        mut pos: usize,
    ) -> bool {
        let last = range.end - 1;
        for i in range.start..last {
            match self.consume_at(elems, i, path, pos, false) {
                Some(end) => pos = end + 1,
                None => return false,
            }
        }
        self.consume_at(elems, last, path, pos, true).is_some()
    }

    /// The head `elems[..g]`, one segment each: where the segment after
    /// it starts. Every globstar wants that segment (`G1` absorbs one,
    /// `G0` is followed by its tail), so a path that ends in the head
    /// fails.
    #[inline]
    fn head<E: ChainElems>(&self, elems: &E, g: usize, path: &[u8]) -> Option<usize> {
        let mut pos = 0;
        for i in 0..g {
            pos = self.consume_at(elems, i, path, pos, false)? + 1;
        }
        Some(pos)
    }

    /// With the head consumed up to `mid_start`: the tail
    /// `elems[g + 1..]` on the last segments, found backward, and the
    /// globstar on what lies between.
    fn tail_and_globstar<E: ChainElems>(
        &self,
        elems: &E,
        g: usize,
        path: &[u8],
        mid_start: usize,
    ) -> bool {
        self.tail_split(elems, g, path, mid_start)
            .is_some_and(|split| self.absorbs(elems.at(g), path, mid_start, split, None))
    }

    /// The tail `elems[g + 1..]` on the last segments, found backward:
    /// where it starts, past the end when there is none; `None` when
    /// it does not fit after `mid_start`.
    #[inline(always)]
    fn tail_split<E: ChainElems>(
        &self,
        elems: &E,
        g: usize,
        path: &[u8],
        mid_start: usize,
    ) -> Option<usize> {
        let n = elems.len();
        let mut split = path.len() + 1;
        let mut end = path.len();
        for j in (g + 1..n).rev() {
            let s = seg_start(path, end);
            if s < mid_start || !self.elem_consumes(elems.at(j), &path[s..end]) {
                return None;
            }
            split = s;
            if j > g + 1 {
                if s == 0 {
                    return None;
                }
                end = s - 1;
            }
        }
        Some(split)
    }

    /// With the head consumed up to `mid_start` and the tail found at
    /// `split`: the block `elems[g1 + 1..g2]` on some run of the segments
    /// between, each globstar on what is left on its side. Under
    /// `dot=false` a globstar takes no dot segment, so the block must
    /// start at or before the first one and none may follow it.
    fn block_between<E: ChainElems>(
        &self,
        elems: &E,
        g1: usize,
        g2: usize,
        path: &[u8],
        mid_start: usize,
        split: usize,
    ) -> bool {
        if split == mid_start {
            return false;
        }
        // The middle's segments end before the tail's separator, or at
        // the end of the path (`split` is then one past it).
        let mid_end = split - 1;
        let mut pos = mid_start;
        loop {
            let end = seg_end(path, pos);
            if let Some(taken) = self.block_from(elems, g1 + 1..g2, path, pos..end, mid_end) {
                // Nothing before the block holds a dot segment (the loop
                // stops at the first); the second globstar checks after.
                let left = pos > mid_start || elems.at(g1).nullable();
                if left && self.absorbs(elems.at(g2), path, taken + 1, split, None) {
                    return true;
                }
            }
            // Past the middle, or at a dot segment the first globstar
            // could not take.
            if end >= mid_end || (!self.dot && path[pos..end].starts_with(b".")) {
                return false;
            }
            pos = end + 1;
        }
    }

    /// `elems[block]` on the segments from `seg`, one each, the last
    /// ending at a separator or at `mid_end`: the index just past it.
    #[inline(always)]
    fn block_from<E: ChainElems>(
        &self,
        elems: &E,
        block: std::ops::Range<usize>,
        path: &[u8],
        seg: std::ops::Range<usize>,
        mid_end: usize,
    ) -> Option<usize> {
        let (from, to) = (block.start, block.end);
        let end = seg.end;
        if !self.elem_consumes(elems.at(from), &path[seg]) {
            return None;
        }
        if to - from == 1 {
            return Some(end);
        }
        if end >= mid_end {
            return None;
        }
        let mut pos = end + 1;
        for i in from + 1..to - 1 {
            pos = self.consume_at(elems, i, path, pos, false)? + 1;
        }
        let last = to - 1;
        match self.consume_at(elems, last, path, pos, false) {
            Some(end) => (end <= mid_end).then_some(end),
            None if mid_end == path.len() => self.consume_at(elems, last, path, pos, true),
            None => None,
        }
    }

    /// Can the globstar `kind` take the segments between the head
    /// (ending before `from`) and the tail starting at `split`?
    /// `split == from` means no segment at all. `dot_at`, when given, is
    /// `first_dot(path, from, ..)`, which makes the dot check one compare.
    #[inline(always)]
    fn absorbs(
        &self,
        kind: &Elem,
        path: &[u8],
        from: usize,
        split: usize,
        dot_at: Option<usize>,
    ) -> bool {
        if split == from {
            return kind.nullable();
        }
        // Up to the separator before the tail, or the end.
        let to = split - 1;
        self.dot || dot_at.unwrap_or_else(|| self.first_dot(path, from, to)) >= to
    }

    /// Under `dot=false`, where the first segment of `path[from..to)`
    /// that starts with a `.` starts; `usize::MAX` when none does, and
    /// under `dot`.
    fn first_dot(&self, path: &[u8], from: usize, to: usize) -> usize {
        if self.dot {
            return usize::MAX;
        }
        if from < to && path[from] == b'.' {
            return from;
        }
        (from..to)
            .find(|&i| {
                std::path::is_separator(path[i] as char) && i + 1 < to && path[i + 1] == b'.'
            })
            .map_or(usize::MAX, |i| i + 1)
    }

    #[inline(always)]
    fn elem_consumes(&self, e: &Elem, seg: &[u8]) -> bool {
        match e {
            Elem::Lit(lit) => self.lit_eq(lit, seg),
            Elem::Wild(w) => self.wild_consumes(w, seg),
            _ => false,
        }
    }
}

// ── match_dir ───────────────────────────────────────────────────────────

impl SegmentMatcher {
    /// `match_dir` through the lone chain of a matcher that keeps no
    /// NFA.
    pub(super) fn chain_dir(&self, dir: &[u8]) -> DirMatch {
        let chain = &self.chains[0];
        let Shape::Line(line) = chain.shape else {
            unreachable!("only a lone chain with at most one globstar serves alone")
        };
        if chain.nodes.is_empty() {
            self.chain_dir_on(line, &&self.elems[..], dir)
        } else {
            let elems = Mapped {
                nodes: &chain.nodes,
                elems: &self.elems,
            };
            self.chain_dir_on(line, &elems, dir)
        }
    }

    /// The tokens the NFA would hold after `dir`, read off the chain: a
    /// head position when the directory ends inside the head, the
    /// globstar when it has taken everything after the head, and a
    /// tail position when the tail's first elements take the last
    /// segments.
    fn chain_dir_on<E: ChainElems>(&self, line: Line, elems: &E, dir: &[u8]) -> DirMatch {
        let n = elems.len();
        let g = line.g as usize;
        let open_from = line.open_from as usize;
        let any_from = line.any_from as usize;
        // A fixed chain: the globstar sits past the end, the tail is
        // empty.
        let tl = n - g - usize::from(g < n);
        // Every element from `i` on takes some segment: a token there
        // still reaches accept.
        let open = |i: usize| i >= open_from;
        let tail_any = n - tl >= any_from;
        if dir.is_empty() {
            // The walk root: nothing consumed yet. All below iff a
            // leading globstar is followed by at most one any-segment.
            let all = self.dot && g == 0 && tl <= 1 && tail_any;
            return DirMatch::new(self.is_match(dir), true, all);
        }
        let mut mid_start = 0;
        for i in 0..g {
            match self.consume_at(elems, i, dir, mid_start, false) {
                Some(end) => mid_start = end + 1,
                None if self.consume_at(elems, i, dir, mid_start, true).is_some() => {
                    // `dir` is the chain's first `k` segments. A globstar
                    // reached with nothing absorbed covers all below iff
                    // its tail is at most one any-segment.
                    let k = i + 1;
                    let all = self.dot && g < n && k == g && tl <= 1 && tail_any;
                    return DirMatch::new(k == n, k < n && open(k), all);
                }
                None => return DirMatch::Pruned,
            }
        }
        if g == n {
            // A fixed chain, and the directory is deeper.
            return DirMatch::Pruned;
        }
        let kind = elems.at(g);
        let exact = self.tail_and_globstar(elems, g, dir, mid_start);
        // The globstar took everything after the head.
        let g_alive = self.absorbs(kind, dir, mid_start, dir.len() + 1, None);
        let mut prefix = g_alive && open(g + 1);
        let mut all = self.dot && g_alive && tail_any;
        if (!prefix || all) && tl > 1 {
            (prefix, all) = self.tail_tokens(elems, line, dir, mid_start, prefix, all);
        }
        DirMatch::new(exact, prefix, all)
    }

    /// `chain_dir`'s tail tokens: `elems[g + 1..g + 1 + j]` on the last
    /// `j` segments of `dir` with the globstar released before them. A
    /// dead globstar may leave one, which sets `prefix`; `all` needs
    /// them all. Each `j` is its own alignment, so the compares are
    /// budgeted. Kept out of `chain_dir_on`, whose short paths lost 20%
    /// with it inside.
    #[inline(never)]
    fn tail_tokens<E: ChainElems>(
        &self,
        elems: &E,
        line: Line,
        dir: &[u8],
        mid_start: usize,
        mut prefix: bool,
        mut all: bool,
    ) -> (bool, bool) {
        let n = elems.len();
        let g = line.g as usize;
        let open_from = line.open_from as usize;
        let tl = n - g - 1;
        let kind = elems.at(g);
        let dot_at = Some(self.first_dot(dir, mid_start, dir.len()));
        let mut checks = MAX_TAIL_CHECKS;
        let mut end = dir.len();
        for j in 1..tl {
            if checks < j {
                return (true, false);
            }
            checks -= j;
            let start = seg_start(dir, end);
            let alive = self.absorbs(kind, dir, mid_start, start, dot_at)
                && self.fixed_run(elems, g + 1..g + 1 + j, dir, start);
            prefix |= alive && g + 1 + j >= open_from;
            all &= alive;
            if start == mid_start {
                // No segment is left for a longer tail.
                all &= j + 1 == tl;
                break;
            }
            if prefix && !all {
                break;
            }
            end = start - 1;
        }
        (prefix, all)
    }
}
