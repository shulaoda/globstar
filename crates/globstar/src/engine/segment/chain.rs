//! The chains: the wildcard paths through the graph, each matched by
//! anchored compares; the literal paths as one sorted set; the index
//! that picks a big union's chains.

use std::cmp::Ordering;
use std::ops::Range;

use crate::dir_match::DirMatch;
use crate::engine::bytes::{cmp_folded, fold, is_sep, seg_end, seg_start};

use super::SegmentMatcher;
use super::bits::ByteSketch;
use super::graph::{ACCEPT, Elem, Graph};
use super::wild::WildKind;
use crate::parse::ast::Node;

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

    /// Does a path of the set start with `dir/`?
    pub(super) fn any_below(&self, dir: &[u8], ci: bool) -> bool {
        if dir.is_empty() {
            return self.len() > 0;
        }
        let (mut lo, mut hi) = (0usize, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.cmp_below(mid, dir, ci) == Ordering::Less {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo < self.len() && self.cmp_below(lo, dir, ci) == Ordering::Equal
    }

    /// Path `k` against `dir/`: `Equal` when it starts with it.
    fn cmp_below(&self, k: usize, dir: &[u8], ci: bool) -> Ordering {
        let lit = &self.bytes[self.bounds[k] as usize..self.bounds[k + 1] as usize];
        let n = lit.len().min(dir.len());
        match cmp_folded(&lit[..n], &dir[..n], ci) {
            Ordering::Equal if lit.len() > dir.len() => lit[dir.len()].cmp(&b'/'),
            Ordering::Equal => Ordering::Less,
            other => other,
        }
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

// The chain index: which chains of a big union a path may match, by the
// literal strings one of its anchored segments must have. A path looks
// up its last, first and inner segments and walks only the chains they
// pick.

/// A union of at most this many chains tries them in turn after their
/// gates, which costs less than the lookups.
pub(super) const LINEAR_CHAINS: usize = 10;

/// A product of alternatives (`[._]s[a-v][a-z]`) is cut to the nodes
/// whose strings fit this many; a single list is kept up to the key room.
const MAX_KEY_PRODUCT: usize = 64;

/// A key compares this many outer bytes of a segment at most; the walk
/// checks the rest. Past 64 chains an exact key compares the whole
/// segment by its hash.
const KEY_BYTES: usize = 4;

/// The base of `Key::whole`.
const BASE: u32 = 0x0100_0193;

/// An infix is at most this many first bytes of a literal.
const MAX_INFIX: usize = 16;

/// The lookups of one role: an exact key (0), a suffix of each length up
/// to `KEY_BYTES` (1 to 4), a prefix of each (5 to 8).
const LOOKUPS: usize = 1 + 2 * KEY_BYTES;

/// The keys a union's segments may give and the entries its index may
/// file: one per unit of the pattern's weight, at least 4,096.
pub(super) fn key_room(weight: usize) -> usize {
    weight.max(4096)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Exact,
    Suffix,
    Prefix,
}

/// A string by its length and bytes, folded when case-insensitive: `back`
/// holds its last `KEY_BYTES` (the last lowest), `front` its first (the
/// first lowest), `whole` a hash of all, `pow` `BASE` to its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    len: usize,
    back: u32,
    front: u32,
    whole: u32,
    pow: u32,
}

#[derive(Debug)]
pub(super) enum SegKeys {
    Strings(Vec<(Kind, Key)>),
    /// Every match holds this literal (`*.so.*` holds `.so.`).
    Infix(Box<[u8]>),
}

#[derive(Debug, Default)]
pub(super) struct KeyScratch {
    suffixes: Vec<Key>,
    prefixes: Vec<Key>,
    alts: Vec<Key>,
    next: Vec<Key>,
}

/// Which segment of a path a chain's key is looked up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// The last: the chain's last element takes it.
    Last = 0,
    /// The first: the chain's first element takes it.
    First = 1,
    /// Any but the last: a block's first element takes one of them.
    Inner = 2,
}

#[derive(Debug, Clone)]
pub(super) struct ChainIndex {
    /// Open addressing by `hash`, at most one entry in two slots: a tag of
    /// the key's hash above the chain id (`id_bits`), 0 where there is none.
    slots: Box<[u32]>,
    shift: u32,
    /// 6 for a union of at most 64 chains, whose picks are bits of a word.
    id_bits: u32,
    /// Per role, the lookups that find something, and per lookup a sketch
    /// of the outer bytes filed.
    lookups: [u16; 3],
    sketch: [[u64; LOOKUPS]; 3],
    /// The chains a literal anywhere in their last segment picks.
    infix: Box<[(u32, Box<[u8]>)]>,
    /// The chains no key files: tried on every call, last.
    rest: Box<[u32]>,
}

impl Key {
    const EMPTY: Key = Key {
        len: 0,
        back: 0,
        front: 0,
        whole: 0,
        pow: 1,
    };

    fn of(bytes: &[u8], ci: bool) -> Key {
        Key {
            len: bytes.len(),
            back: pack(bytes.iter().rev().copied(), ci),
            front: pack(bytes.iter().copied(), ci),
            whole: whole_of(bytes, ci),
            pow: BASE.wrapping_pow(bytes.len() as u32),
        }
    }

    fn then(self, next: Key) -> Key {
        let back = if next.len >= KEY_BYTES {
            next.back
        } else {
            next.back | self.back << (8 * next.len)
        };
        let front = if self.len >= KEY_BYTES {
            self.front
        } else {
            self.front | next.front << (8 * self.len)
        };
        Key {
            len: self.len + next.len,
            back,
            front,
            whole: self.whole.wrapping_mul(next.pow).wrapping_add(next.whole),
            pow: self.pow.wrapping_mul(next.pow),
        }
    }
}

impl SegKeys {
    /// What it takes of the key room.
    pub(super) fn len(&self) -> usize {
        match self {
            SegKeys::Strings(keys) => keys.len(),
            SegKeys::Infix(_) => 1,
        }
    }
}

/// The keys of a segment's nodes, none when they pass `room`. A segment
/// that is one brace bound apart by its branches (`{Makefile,inc.*,*.pc}`)
/// is bound by all their keys, each its own kind.
pub(super) fn segment_keys(
    nodes: &[&Node],
    ci: bool,
    room: usize,
    k: &mut KeyScratch,
) -> Option<SegKeys> {
    let keys = one_kind(nodes, ci, room, k);
    let (None, [Node::Brace(branches)]) = (&keys, nodes) else {
        return keys.filter(|keys| keys.len() <= room);
    };
    let mut out = Vec::new();
    for branch in branches {
        let seq: Vec<&Node> = branch.seq().iter().collect();
        match segment_keys(&seq, ci, room - out.len(), k)? {
            SegKeys::Strings(keys) => out.extend(keys),
            SegKeys::Infix(_) => return None,
        }
    }
    Some(SegKeys::Strings(out))
}

/// One kind of keys: the segment's strings when it is literal; else those
/// of the literal nodes that end it or start it, whichever bind more
/// bytes; else the start of its longest literal. None when it has no
/// literal, or a string may be empty.
fn one_kind(nodes: &[&Node], ci: bool, room: usize, k: &mut KeyScratch) -> Option<SegKeys> {
    let KeyScratch {
        suffixes,
        prefixes,
        alts,
        next,
    } = k;
    let from_end = nodes.iter().rev().copied();
    let taken = literal_run(from_end, true, ci, room, suffixes, alts, next);
    if taken == nodes.len() {
        return strings(Kind::Exact, suffixes);
    }
    literal_run(nodes.iter().copied(), false, ci, room, prefixes, alts, next);
    let shortest = |keys: &[Key]| keys.iter().map(|k| k.len).min().unwrap_or(0);
    match (shortest(suffixes), shortest(prefixes)) {
        (0, 0) => {
            let longest = nodes
                .iter()
                .filter_map(|n| match n {
                    Node::Literal(bytes) => Some(bytes),
                    _ => None,
                })
                .max_by_key(|bytes| bytes.len())?;
            Some(SegKeys::Infix(
                longest
                    .iter()
                    .take(MAX_INFIX)
                    .map(|&b| fold(b, ci))
                    .collect(),
            ))
        }
        (s, p) if p > s => strings(Kind::Prefix, prefixes),
        _ => strings(Kind::Suffix, suffixes),
    }
}

fn strings(kind: Kind, keys: &[Key]) -> Option<SegKeys> {
    (!keys.iter().any(|k| k.len == 0))
        .then(|| SegKeys::Strings(keys.iter().map(|&k| (kind, k)).collect()))
}

/// The strings of the literal nodes `nodes` yields from one end, as many
/// as fit `MAX_KEY_PRODUCT`, into `out`; how many nodes they took. A
/// node of one string waits in `fixed` for the next choice.
fn literal_run<'a>(
    nodes: impl Iterator<Item = &'a Node>,
    back: bool,
    ci: bool,
    room: usize,
    out: &mut Vec<Key>,
    alts: &mut Vec<Key>,
    next: &mut Vec<Key>,
) -> usize {
    let join = |key: Key, node: Key| if back { node.then(key) } else { key.then(node) };
    let flush = |out: &mut Vec<Key>, fixed: &mut Key| {
        if fixed.len > 0 {
            for o in out.iter_mut() {
                *o = join(*o, *fixed);
            }
            *fixed = Key::EMPTY;
        }
    };
    out.clear();
    out.push(Key::EMPTY);
    let mut fixed = Key::EMPTY;
    let mut taken = 0;
    for node in nodes {
        alts.clear();
        if !alternatives(node, ci, room, alts) {
            break;
        }
        if let [one] = alts[..] {
            fixed = join(fixed, one);
        } else {
            if out.len() > 1 && out.len() * alts.len() > MAX_KEY_PRODUCT {
                break;
            }
            flush(out, &mut fixed);
            next.clear();
            for &o in out.iter() {
                next.extend(alts.iter().map(|&a| join(o, a)));
            }
            std::mem::swap(out, next);
        }
        taken += 1;
    }
    flush(out, &mut fixed);
    taken
}

/// The strings one literal node matches: a literal, a positive ASCII
/// class, a brace of literal branches. `false` for any other node, and
/// for a brace of more than `room` strings.
fn alternatives(node: &Node, ci: bool, room: usize, out: &mut Vec<Key>) -> bool {
    match node {
        Node::Literal(bytes) => out.push(Key::of(bytes, ci)),
        Node::Class(class) if !class.negated => {
            for &(lo, hi) in &class.ranges {
                if !hi.is_ascii()
                    || out.len() + usize::from(hi.saturating_sub(lo)) >= MAX_KEY_PRODUCT
                {
                    return false;
                }
                out.extend((lo..=hi).map(|b| Key::of(&[b], ci)));
            }
        }
        Node::Brace(branches) => {
            for branch in branches {
                match branch.seq() {
                    [Node::Literal(bytes)] => out.push(Key::of(bytes, ci)),
                    seq => {
                        let (mut keys, mut alts, mut next) = (Vec::new(), Vec::new(), Vec::new());
                        let taken = literal_run(
                            seq.iter(),
                            false,
                            ci,
                            room,
                            &mut keys,
                            &mut alts,
                            &mut next,
                        );
                        if taken < seq.len() {
                            return false;
                        }
                        out.extend_from_slice(&keys);
                    }
                }
                if out.len() > room {
                    return false;
                }
            }
        }
        _ => return false,
    }
    true
}

impl ChainIndex {
    /// `None` when the chains are few, their entries pass the key room, or
    /// past 64 chains a call could walk more than `MAX_CHAINS` of them.
    pub(super) fn new(
        literals: Option<&Literals>,
        chains: &[Chain],
        graph: &Graph,
        ci: bool,
    ) -> Option<Box<Self>> {
        let (elems, keys) = (&graph.elems, &graph.keys);
        if chains.len() <= LINEAR_CHAINS {
            return None;
        }
        // The literal set is filed as id 0, chain `c` as `c + first_chain`.
        let first_chain = usize::from(literals.is_some());
        let ids = chains.len() + first_chain;
        let mut index = ChainIndex {
            slots: Box::default(),
            shift: 0,
            id_bits: (usize::BITS - (ids - 1).leading_zeros()).max(6),
            lookups: [0; 3],
            sketch: [[0; LOOKUPS]; 3],
            infix: Box::default(),
            rest: Box::default(),
        };
        let mut entries = Vec::new();
        let mut infix = Vec::new();
        let mut rest = Vec::new();
        if let Some(set) = literals {
            for path in set.iter() {
                let first = path.split(|&b| b == b'/').next().unwrap_or_default();
                index.file(
                    Role::First,
                    Kind::Exact,
                    Key::of(first, ci),
                    0,
                    &mut entries,
                );
            }
        }
        for (c, chain) in chains.iter().enumerate() {
            let id = c + first_chain;
            let mut filed = false;
            for (role, node) in chain.anchors() {
                match &elems[node as usize] {
                    Elem::Lit(lit) if !lit.is_empty() => {
                        index.file(role, Kind::Exact, Key::of(lit, ci), id, &mut entries);
                        filed = true;
                    }
                    Elem::Wild(_) => {
                        if let Ok(i) = keys.binary_search_by_key(&node, |&(n, _)| n) {
                            match &keys[i].1 {
                                SegKeys::Strings(strings) => {
                                    for &(kind, key) in strings {
                                        index.file(role, kind, key, id, &mut entries);
                                    }
                                    filed = true;
                                }
                                SegKeys::Infix(lit) if role == Role::Last => {
                                    infix.push((id as u32, lit.clone()));
                                    filed = true;
                                }
                                SegKeys::Infix(_) => {}
                            }
                        }
                    }
                    _ => {}
                }
                if filed {
                    break;
                }
            }
            if !filed {
                rest.push(id as u32);
            }
            if entries.len() > key_room(graph.weight) {
                return None;
            }
        }
        entries.sort_unstable();
        entries.dedup();
        if index.id_bits > 6
            && (rest.len() + infix.len() > MAX_CHAINS
                || entries
                    .chunk_by(|a, b| a.0 == b.0)
                    .any(|g| g.len() > MAX_CHAINS))
        {
            return None;
        }
        let bits = (2 * entries.len())
            .next_power_of_two()
            .trailing_zeros()
            .max(1);
        index.shift = 64 - bits;
        let mut slots = vec![0u32; 1 << bits];
        let last = slots.len() - 1;
        for (key, c) in entries {
            let (mut at, tag) = index.hash(key);
            while slots[at] != 0 {
                at = (at + 1) & last;
            }
            slots[at] = tag | c;
        }
        index.slots = slots.into_boxed_slice();
        index.infix = infix.into_boxed_slice();
        index.rest = rest.into_boxed_slice();
        Some(Box::new(index))
    }

    fn file(&mut self, role: Role, kind: Kind, key: Key, c: usize, entries: &mut Vec<(u64, u32)>) {
        let short = key.len.min(KEY_BYTES);
        let (lookup, len, outer, compared) = match kind {
            Kind::Exact => (
                0,
                key.len,
                key.back,
                self.exact(key.back, key.len, || key.whole),
            ),
            Kind::Suffix => (short, 0, key.back, key.back & mask(short)),
            Kind::Prefix => (KEY_BYTES + short, 0, key.front, key.front & mask(short)),
        };
        let r = role as usize;
        self.lookups[r] |= 1 << lookup;
        self.sketch[r][lookup] |= 1 << (outer & 63);
        entries.push((lookup_key(role, lookup, len, compared), c as u32));
    }

    /// What an exact key compares: its outer bytes, or past 64 chains the
    /// whole string, so that `n17.js` and `n27.js` file apart.
    #[inline]
    fn exact(&self, back: u32, len: usize, whole: impl FnOnce() -> u32) -> u32 {
        if self.id_bits > 6 {
            whole()
        } else {
            back & mask(len)
        }
    }

    /// A key's first slot, and its tag: the hash bits the slot does not
    /// use, the top one set.
    fn hash(&self, key: u64) -> (usize, u32) {
        let h = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let tag = ((h >> 8) as u32 | 1 << (31 - self.id_bits)) << self.id_bits;
        ((h >> self.shift) as usize, tag)
    }

    /// Does a chain the path picks match it? The last segment's picks are
    /// tried first, then the first's, the inner ones', then the chains
    /// without a key; each chain once.
    pub(super) fn any(
        &self,
        path: &[u8],
        ci: bool,
        mut matches: impl FnMut(usize) -> bool,
    ) -> bool {
        if self.id_bits > 6 {
            return self.any_of_many(path, ci, matches);
        }
        let mut tried = 0u64;
        macro_rules! try_picked {
            ($picked:expr) => {
                let mut todo = $picked & !tried;
                tried |= todo;
                while todo != 0 {
                    let c = todo.trailing_zeros() as usize;
                    todo &= todo - 1;
                    if matches(c) {
                        return true;
                    }
                }
            };
        }
        let last_start = seg_start(path, path.len());
        let last = &path[last_start..];
        let mut picked = 0u64;
        self.probe(Role::Last, last, ci, |c| picked |= 1 << c);
        for c in self.infix_picks(last, ci) {
            picked |= 1 << c;
        }
        try_picked!(picked);
        if self.lookups[Role::First as usize] != 0 {
            let mut picked = 0u64;
            self.probe(Role::First, &path[..seg_end(path, 0)], ci, |c| {
                picked |= 1 << c
            });
            try_picked!(picked);
        }
        if self.lookups[Role::Inner as usize] != 0 {
            let mut start = 0;
            while start < last_start {
                let end = seg_end(path, start);
                let mut picked = 0u64;
                self.probe(Role::Inner, &path[start..end], ci, |c| picked |= 1 << c);
                try_picked!(picked);
                start = end + 1;
            }
        }
        self.rest.iter().any(|&c| matches(c as usize))
    }

    /// `any` past 64 chains: each step's picks sorted and tried once.
    fn any_of_many(&self, path: &[u8], ci: bool, mut matches: impl FnMut(usize) -> bool) -> bool {
        let mut try_picked = |picked: &mut Vec<u32>| {
            picked.sort_unstable();
            picked.dedup();
            let hit = picked.iter().any(|&c| matches(c as usize));
            picked.clear();
            hit
        };
        let mut picked = Vec::new();
        let last_start = seg_start(path, path.len());
        let last = &path[last_start..];
        self.probe(Role::Last, last, ci, |c| picked.push(c));
        picked.extend(self.infix_picks(last, ci));
        if try_picked(&mut picked) {
            return true;
        }
        if self.lookups[Role::First as usize] != 0 {
            self.probe(Role::First, &path[..seg_end(path, 0)], ci, |c| {
                picked.push(c)
            });
            if try_picked(&mut picked) {
                return true;
            }
        }
        if self.lookups[Role::Inner as usize] != 0 {
            let mut start = 0;
            while start < last_start {
                let end = seg_end(path, start);
                self.probe(Role::Inner, &path[start..end], ci, |c| picked.push(c));
                start = end + 1;
            }
        }
        picked.extend_from_slice(&self.rest);
        try_picked(&mut picked)
    }

    fn infix_picks<'a>(&'a self, last: &'a [u8], ci: bool) -> impl Iterator<Item = u32> + 'a {
        self.infix
            .iter()
            .filter(move |(_, lit)| {
                last.windows(lit.len())
                    .any(|w| w.iter().zip(lit).all(|(&a, &b)| fold(a, ci) == b))
            })
            .map(|&(c, _)| c)
    }

    /// Hands `pick` the chains `seg` picks for keys of `role`.
    #[inline]
    fn probe(&self, role: Role, seg: &[u8], ci: bool, mut pick: impl FnMut(u32)) {
        let r = role as usize;
        let n = seg.len();
        let mut todo = self.lookups[r];
        let back = pack(seg.iter().rev().copied(), ci);
        let front = if todo >> (KEY_BYTES + 1) != 0 {
            pack(seg.iter().copied(), ci)
        } else {
            0
        };
        let ids = (1 << self.id_bits) - 1;
        while todo != 0 {
            let lookup = todo.trailing_zeros() as usize;
            todo &= todo - 1;
            let (compared, outer) = if lookup == 0 {
                (n, back)
            } else if lookup <= KEY_BYTES {
                (lookup, back)
            } else {
                (lookup - KEY_BYTES, front)
            };
            if compared > n || self.sketch[r][lookup] & 1 << (outer & 63) == 0 {
                continue;
            }
            let key = if lookup == 0 {
                lookup_key(role, 0, n, self.exact(back, n, || whole_of(seg, ci)))
            } else {
                lookup_key(role, lookup, 0, outer & mask(compared))
            };
            let (mut at, tag) = self.hash(key);
            let last = self.slots.len() - 1;
            loop {
                let slot = self.slots[at];
                if slot == 0 {
                    break;
                }
                if slot & !ids == tag {
                    pick(slot & ids);
                }
                at = (at + 1) & last;
            }
        }
    }
}

/// A role, a lookup, an exact key's length (past 255 as 255) and what it
/// compares, in one word.
fn lookup_key(role: Role, lookup: usize, len: usize, compared: u32) -> u64 {
    (role as u64) << 48 | (lookup as u64) << 40 | (len.min(255) as u64) << 32 | u64::from(compared)
}

/// The folded bytes as a polynomial in `BASE`, the last byte its constant term.
fn whole_of(bytes: &[u8], ci: bool) -> u32 {
    bytes.iter().fold(0, |h: u32, &b| {
        h.wrapping_mul(BASE).wrapping_add(u32::from(fold(b, ci)))
    })
}

/// The first `KEY_BYTES` of `bytes`, folded, the first lowest.
fn pack(bytes: impl Iterator<Item = u8>, ci: bool) -> u32 {
    let mut w = 0;
    for (i, b) in bytes.take(KEY_BYTES).enumerate() {
        w |= u32::from(fold(b, ci)) << (8 * i);
    }
    w
}

fn mask(len: usize) -> u32 {
    if len >= KEY_BYTES {
        u32::MAX
    } else {
        (1 << (8 * len)) - 1
    }
}
