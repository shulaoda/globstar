//! The chains: the wildcard paths through the graph, each matched by
//! anchored compares; the literal paths as one sorted set.

use std::cmp::Ordering;
use std::ops::Range;

use crate::bytes::{cmp_folded, fold, is_sep, seg_end, seg_start};
use crate::dir_match::DirMatch;

use super::SegmentMatcher;
use super::bits::ByteSketch;
use super::chain_index::{ChainIndex, Role};
use super::graph::{ACCEPT, Elem, Graph};
use super::wild::WildKind;

/// Wildcard chains kept without an index; more while their nodes fit
/// four times the pattern's weight.
pub(super) const MAX_CHAINS: usize = 64;

/// The literal set's bytes, one more per path, or four times the weight.
const MAX_LITERAL_BYTES: usize = 64 * 1024;

/// `chain_dir`'s budget of element compares on tail tokens.
const MAX_TAIL_CHECKS: usize = 4096;

const NO_HEAD: u8 = 0;

/// The graph's paths: the literal ones as one set, the others as chains.
#[derive(Default)]
pub(super) struct Paths {
    pub(super) literals: Option<Box<Literals>>,
    pub(super) chains: Box<[Chain]>,
}

#[derive(Debug, Clone)]
pub(super) struct Chain {
    /// Empty for a linear pattern's lone path (`0..elems.len()`).
    pub(super) nodes: Box<[u32]>,
    pub(super) shape: Shape,
}

#[derive(Debug, Clone)]
pub(super) enum Shape {
    Line(Line),
    Block { g1: u32, g2: u32 },
}

/// A globstar at `g` (past the end: none). From `open_from` every
/// element takes some segment, from `any_from` any non-empty one; both
/// 0 on a union member's chain, which `match_dir` never reads.
#[derive(Debug, Clone, Copy)]
pub(super) struct Line {
    g: u32,
    open_from: u32,
    any_from: u32,
}

/// Sorted paths laid end to end: path `k` is `bytes[bounds[k]..bounds[k + 1]]`.
#[derive(Debug, Clone)]
pub(super) struct Literals {
    bytes: Box<[u8]>,
    bounds: Box<[u32]>,
}

/// The bytes a match may end with, and the folded first byte a literal
/// head requires (`NO_HEAD`: none).
#[derive(Debug, Clone, Copy)]
pub(super) struct Gate {
    last: ByteSketch,
    head: u8,
}

impl Graph {
    /// `None` past the caps or when a brace cut a segment into pieces.
    pub(super) fn paths(&self, ci: bool) -> Option<Paths> {
        if self.glue.is_some() {
            return None;
        }
        let Some(edges) = &self.edges else {
            return Some(Paths {
                literals: None,
                chains: self.chain_of(None, true).into_iter().collect(),
            });
        };
        let mut wild_paths: Vec<Vec<u32>> = Vec::new();
        let mut wild_nodes = 0;
        let mut literal_bytes: Vec<u8> = Vec::new();
        let mut literal_bounds: Vec<u32> = vec![0];
        let mut path: Vec<u32> = Vec::new();
        let mut stack: Vec<(&[u32], usize)> = vec![(edges.succ(self.elems.len()), 0)];
        while let Some((succ, next)) = stack.last_mut() {
            let Some(&id) = succ.get(*next) else {
                stack.pop();
                path.pop();
                continue;
            };
            *next += 1;
            if id == ACCEPT {
                let nodes: Vec<u32> = path
                    .iter()
                    .copied()
                    .filter(|&i| !matches!(self.elems[i as usize], Elem::Join))
                    .collect();
                let literal = nodes
                    .iter()
                    .all(|&i| matches!(self.elems[i as usize], Elem::Lit(_)));
                if literal {
                    self.literal_path(&nodes, &mut literal_bytes);
                    literal_bounds.push(literal_bytes.len() as u32);
                } else {
                    wild_nodes += nodes.len();
                    wild_paths.push(nodes);
                }
                if wild_paths.len() > MAX_CHAINS && wild_nodes > 4 * self.weight
                    || literal_bytes.len() + literal_bounds.len()
                        > MAX_LITERAL_BYTES.max(4 * self.weight)
                {
                    return None;
                }
                continue;
            }
            path.push(id);
            stack.push((edges.succ(id as usize), 0));
        }
        let lone = wild_paths.len() == 1 && literal_bounds.len() == 1;
        let chains = wild_paths
            .into_iter()
            .map(|p| self.chain_of(Some(p), lone))
            .collect::<Option<_>>()?;
        let literals = (literal_bounds.len() > 1)
            .then(|| Box::new(Literals::new(&literal_bytes, &literal_bounds, ci)));
        Some(Paths { literals, chains })
    }

    fn literal_path(&self, path: &[u32], out: &mut Vec<u8>) {
        for (k, &i) in path.iter().enumerate() {
            if k > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(self.elems[i as usize].as_lit());
        }
    }

    fn chain_of(&self, path: Option<Vec<u32>>, lone: bool) -> Option<Chain> {
        let n = path.as_ref().map_or(self.elems.len(), Vec::len);
        let elem = |i: usize| &self.elems[path.as_ref().map_or(i, |p| p[i] as usize)];
        let mut globstars = (0..n).filter(|&i| elem(i).is_globstar());
        let first_from = |ok: fn(&Elem) -> bool| {
            (0..n).rev().find(|&j| !ok(elem(j))).map_or(0, |j| j + 1) as u32
        };
        let shape = match (globstars.next(), globstars.next(), globstars.next()) {
            (Some(g1), Some(g2), None) if g2 > g1 + 1 => Shape::Block {
                g1: g1 as u32,
                g2: g2 as u32,
            },
            (Some(_), Some(_), _) => return None,
            (g, ..) => Shape::Line(Line {
                g: g.unwrap_or(n) as u32,
                open_from: if lone {
                    first_from(Elem::satisfiable)
                } else {
                    0
                },
                any_from: if lone { first_from(Elem::any_seg) } else { 0 },
            }),
        };
        Some(Chain {
            nodes: path.map_or_else(Default::default, Vec::into_boxed_slice),
            shape,
        })
    }
}

impl Chain {
    /// The nodes whose segment the shape pins, in the index's lookup
    /// order: the last, the first, a block's first.
    pub(super) fn anchors(&self) -> impl Iterator<Item = (Role, u32)> + '_ {
        let n = self.nodes.len();
        let (head, tail, block) = match self.shape {
            Shape::Line(line) => (line.g as usize, (line.g as usize + 1).min(n), None),
            Shape::Block { g1, g2 } => (g1 as usize, g2 as usize + 1, Some(g1 as usize + 1)),
        };
        let last = (tail < n || head == n && n > 0).then(|| (Role::Last, self.nodes[n - 1]));
        let first = (head > 0).then(|| (Role::First, self.nodes[0]));
        let inner = block.map(|b| (Role::Inner, self.nodes[b]));
        last.into_iter().chain(first).chain(inner)
    }

    pub(super) fn gate(&self, elems: &[Elem], ci: bool) -> Gate {
        let first = self.nodes.first().map_or(0, |&n| n as usize);
        let last = self.nodes.last().map_or(elems.len() - 1, |&n| n as usize);
        let last = match &elems[last] {
            Elem::Lit(lit) => ByteSketch::of_last_bytes(std::iter::once(&lit[..]), ci),
            Elem::Wild(w) => match &w.kind {
                WildKind::Affix(affix) => {
                    ByteSketch::of_last_bytes(std::iter::once(affix.parts().1), ci)
                }
                WildKind::Suffixes(set) => set.last,
                WildKind::Pieces(_) | WildKind::Generic(_) => ByteSketch::ALL,
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

impl Literals {
    fn new(raw: &[u8], raw_bounds: &[u32], ci: bool) -> Self {
        let at = |k: usize| &raw[raw_bounds[k] as usize..raw_bounds[k + 1] as usize];
        let mut order: Vec<usize> = (0..raw_bounds.len() - 1).collect();
        order.sort_unstable_by(|&a, &b| cmp_folded(at(a), at(b), ci));
        order.dedup_by(|&mut a, &mut b| cmp_folded(at(a), at(b), ci) == Ordering::Equal);
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

    pub(super) fn iter(&self) -> impl Iterator<Item = &[u8]> {
        self.bounds
            .windows(2)
            .map(|w| &self.bytes[w[0] as usize..w[1] as usize])
    }

    pub(super) fn contains(&self, path: &[u8], ci: bool) -> bool {
        let (mut lo, mut hi) = (0usize, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            // One bounds check for the pair: 2% of a set row's call.
            let [start, end] = self.bounds[mid..mid + 2] else {
                unreachable!("mid < len");
            };
            let lit = &self.bytes[start as usize..end as usize];
            match cmp_folded(lit, path, ci) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return true,
            }
        }
        false
    }
}

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

struct Indexed<'a> {
    nodes: &'a [u32],
    elems: &'a [Elem],
}

impl ChainElems for Indexed<'_> {
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
    /// Out of line: keeps `is_match` small for the other matchers.
    #[inline(never)]
    pub(super) fn any_chain_matches(
        &self,
        gates: &[Gate],
        literals: Option<&Literals>,
        path: &[u8],
    ) -> bool {
        if literals.is_some_and(|set| set.contains(path, self.case_insensitive)) {
            return true;
        }
        let first = path
            .first()
            .map_or(NO_HEAD, |&b| fold(b, self.case_insensitive));
        let mut last_start = None;
        self.chains.iter().zip(gates).any(|(chain, gate)| {
            gate.last.may_end(path)
                && (gate.head == NO_HEAD || gate.head == first)
                && self.ends_like(chain, path, &mut last_start)
                && self.chain_matches(chain, path)
        })
    }

    /// The index names the literal set id 0 and the chains the ids after
    /// it, so a step's picks try the set first.
    #[inline(never)]
    pub(super) fn indexed_chain_matches(
        &self,
        index: &ChainIndex,
        literals: Option<&Literals>,
        path: &[u8],
    ) -> bool {
        let ci = self.case_insensitive;
        index.any(path, ci, |id| match literals {
            None => self.chain_matches(&self.chains[id], path),
            Some(set) if id == 0 => set.contains(path, ci),
            Some(_) => self.chain_matches(&self.chains[id - 1], path),
        })
    }

    pub(super) fn chain_matches(&self, chain: &Chain, path: &[u8]) -> bool {
        if chain.nodes.is_empty() {
            self.chain_matches_on(chain, &&self.elems[..], path)
        } else {
            let elems = Indexed {
                nodes: &chain.nodes,
                elems: &self.elems,
            };
            self.chain_matches_on(chain, &elems, path)
        }
    }

    pub(super) fn chain_dir(&self, dir: &[u8]) -> DirMatch {
        let chain = &self.chains[0];
        let Shape::Line(line) = chain.shape else {
            unreachable!("only a lone chain with at most one globstar serves alone")
        };
        if chain.nodes.is_empty() {
            self.chain_dir_on(line, &&self.elems[..], dir)
        } else {
            let elems = Indexed {
                nodes: &chain.nodes,
                elems: &self.elems,
            };
            self.chain_dir_on(line, &elems, dir)
        }
    }

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
            WildKind::Suffixes(_) | WildKind::Pieces(_) | WildKind::Generic(_) => true,
        }
    }

    #[inline]
    fn tail_is(&self, path: &[u8], suffix: &[u8]) -> bool {
        path.len() >= suffix.len() && self.affix_eq(suffix, &path[path.len() - suffix.len()..])
    }

    #[inline]
    fn chain_matches_on<E: ChainElems>(&self, chain: &Chain, elems: &E, path: &[u8]) -> bool {
        match &chain.shape {
            Shape::Line(line) if line.g as usize == elems.len() => {
                self.fixed_run(elems, 0..elems.len(), path, 0)
            }
            Shape::Line(line) => {
                let g = line.g as usize;
                self.run(elems, 0..g, path, 0)
                    .is_some_and(|mid_start| self.tail_and_globstar(elems, g, path, mid_start))
            }
            Shape::Block { g1, g2 } => {
                let (g1, g2) = (*g1 as usize, *g2 as usize);
                let Some(mid_start) = self.run(elems, 0..g1, path, 0) else {
                    return false;
                };
                let Some(split) = self.tail_split(elems, g2, path, mid_start) else {
                    return false;
                };
                self.block_between(elems, g1, g2, path, mid_start, split)
            }
        }
    }

    /// `Some(end)`: `elems[i]` takes the segment at `pos`, the path's
    /// last one iff `last`, and `end` is the index just past it.
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
                    end < path.len() && is_sep(path[end])
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

    #[inline(always)]
    fn elem_consumes(&self, e: &Elem, seg: &[u8]) -> bool {
        match e {
            Elem::Lit(lit) => self.lit_eq(lit, seg),
            Elem::Wild(w) => self.wild_consumes(w, seg),
            _ => false,
        }
    }

    /// Where the segment after `elems[range]` starts, each of them
    /// followed by a separator.
    #[inline]
    fn run<E: ChainElems>(
        &self,
        elems: &E,
        range: Range<usize>,
        path: &[u8],
        mut pos: usize,
    ) -> Option<usize> {
        for i in range {
            pos = self.consume_at(elems, i, path, pos, false)? + 1;
        }
        Some(pos)
    }

    fn fixed_run<E: ChainElems>(
        &self,
        elems: &E,
        range: Range<usize>,
        path: &[u8],
        pos: usize,
    ) -> bool {
        let last = range.end - 1;
        self.run(elems, range.start..last, path, pos)
            .is_some_and(|pos| self.consume_at(elems, last, path, pos, true).is_some())
    }

    /// Where the tail `elems[g + 1..]` starts on the last segments; past
    /// the end when there is none.
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

    /// Under `dot=false` a globstar takes no dot segment, so the block
    /// must start at or before the first one and none may follow it.
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
        let mid_end = split - 1;
        let mut dot_at = 0;
        let mut pos = mid_start;
        loop {
            let end = seg_end(path, pos);
            if let Some(block_end) = self.block_from(elems, g1 + 1..g2, path, pos..end, mid_end) {
                let left_absorbed = pos > mid_start || matches!(elems.at(g1), Elem::G0);
                let from = block_end + 1;
                if left_absorbed {
                    if dot_at < from {
                        dot_at = self.first_dot(path, from, mid_end);
                    }
                    if self.absorbs(elems.at(g2), path, from, split, Some(dot_at)) {
                        return true;
                    }
                }
            }
            if end >= mid_end || (!self.dot && path[pos..end].starts_with(b".")) {
                return false;
            }
            pos = end + 1;
        }
    }

    /// `elems[block]` on the segments from `seg`, the last ending at a
    /// separator or at `mid_end`: the index just past it.
    #[inline(always)]
    fn block_from<E: ChainElems>(
        &self,
        elems: &E,
        block: Range<usize>,
        path: &[u8],
        seg: Range<usize>,
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
        let pos = self.run(elems, from + 1..to - 1, path, end + 1)?;
        let last = to - 1;
        match self.consume_at(elems, last, path, pos, false) {
            Some(end) => (end <= mid_end).then_some(end),
            None if mid_end == path.len() => self.consume_at(elems, last, path, pos, true),
            None => None,
        }
    }

    /// Can the globstar take the segments between `from` and `split`?
    /// `dot_at`, when given, is `first_dot(path, from, ..)`.
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
            return matches!(kind, Elem::G0);
        }
        let to = split - 1;
        self.dot || dot_at.unwrap_or_else(|| self.first_dot(path, from, to)) >= to
    }

    /// Where the first dot-led segment of `path[from..to)` starts;
    /// `usize::MAX` when none does, and under `dot`.
    fn first_dot(&self, path: &[u8], from: usize, to: usize) -> usize {
        if self.dot {
            return usize::MAX;
        }
        if from < to && path[from] == b'.' {
            return from;
        }
        (from..to)
            .find(|&i| is_sep(path[i]) && i + 1 < to && path[i + 1] == b'.')
            .map_or(usize::MAX, |i| i + 1)
    }

    /// The tokens the NFA would hold after `dir`: a head position when
    /// `dir` ends inside the head, the globstar when it took everything
    /// after the head, a tail position when the tail's first elements
    /// take the last segments.
    fn chain_dir_on<E: ChainElems>(&self, line: Line, elems: &E, dir: &[u8]) -> DirMatch {
        let n = elems.len();
        let g = line.g as usize;
        let open_from = line.open_from as usize;
        let any_from = line.any_from as usize;
        let tail_len = n - g - usize::from(g < n);
        let open = |i: usize| i >= open_from;
        let tail_any = n - tail_len >= any_from;
        if dir.is_empty() {
            let all = self.dot && g == 0 && tail_len <= 1 && tail_any;
            return DirMatch::new(self.is_match(dir), true, all);
        }
        let mut mid_start = 0;
        for i in 0..g {
            match self.consume_at(elems, i, dir, mid_start, false) {
                Some(end) => mid_start = end + 1,
                None if self.consume_at(elems, i, dir, mid_start, true).is_some() => {
                    let k = i + 1;
                    let all = self.dot && g < n && k == g && tail_len <= 1 && tail_any;
                    return DirMatch::new(k == n, k < n && open(k), all);
                }
                None => return DirMatch::Pruned,
            }
        }
        if g == n {
            return DirMatch::Pruned;
        }
        let kind = elems.at(g);
        let exact = self.tail_and_globstar(elems, g, dir, mid_start);
        let globstar_alive = self.absorbs(kind, dir, mid_start, dir.len() + 1, None);
        let mut prefix = globstar_alive && open(g + 1);
        let mut all = self.dot && globstar_alive && tail_any;
        if (!prefix || all) && tail_len > 1 {
            (prefix, all) = self.tail_tokens(elems, line, dir, mid_start, prefix, all);
        }
        DirMatch::new(exact, prefix, all)
    }

    /// `elems[g + 1..g + 1 + j]` on the last `j` segments, for each `j`,
    /// within `MAX_TAIL_CHECKS`.
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
        let tail_len = n - g - 1;
        let kind = elems.at(g);
        let dot_at = Some(self.first_dot(dir, mid_start, dir.len()));
        let mut checks = MAX_TAIL_CHECKS;
        let mut end = dir.len();
        for j in 1..tail_len {
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
                all &= j + 1 == tail_len;
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
