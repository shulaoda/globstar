//! The chains: every path through the graph, matched by anchored
//! compares when it has at most one globstar, and `match_dir` read off
//! a lone chain.
//!
//! The predicates that run once per segment or per element
//! (`consume_at`, `elem_consumes`, `absorbs`) are forced inline: LLVM
//! leaves them out of line on its own, and a call there costs 10–40%
//! on the shortest paths. Everything else is left to the compiler.

use crate::dir_match::DirMatch;

use super::graph::{ACCEPT, Graph};
use super::nfa::{Nfa, dense_nfa};
use super::wild::WildKind;
use super::{Elem, SegmentMatcher, seg_end, seg_start};

/// Paths through the graph `is_match` tries one by one; past that the
/// NFA over the whole graph runs.
const MAX_CHAINS: usize = 64;

/// The element compares `chain_dir` spends on tail tokens; past them it
/// answers what is safe ("may match below", "not all below"). Tails of
/// up to 90 elements stay exact.
const MAX_TAIL_CHECKS: usize = 4096;

/// One path through the graph.
#[derive(Debug, Clone)]
pub(super) struct Chain {
    /// Its nodes in order; empty for the lone path of a pattern
    /// without a separator-crossing brace, whose nodes are
    /// `0..elems.len()`.
    pub(super) nodes: Box<[u32]>,
    pub(super) shape: Shape,
}

/// Where a chain's globstars are, which decides how it is matched.
#[derive(Debug, Clone)]
pub(super) enum Shape {
    /// At most one globstar: the head and the tail are anchored around
    /// it.
    Line(Line),
    /// Several: the NFA over this chain alone (a union member; a lone
    /// chain with several globstars is the graph, whose NFA serves).
    Many(Box<Nfa<u64>>),
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

impl Graph {
    /// Every path through the graph as a `Chain`, in pattern order;
    /// none when there are more than `MAX_CHAINS`, or a path with
    /// several globstars is the only path or has more than 64 states
    /// (the NFA takes those).
    pub(super) fn chains(&self) -> Box<[Chain]> {
        let Some(edges) = &self.edges else {
            return self.chain_of(None, true).into_iter().collect();
        };
        let mut paths: Vec<Vec<u32>> = Vec::new();
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
                paths.push(path.clone());
                if paths.len() > MAX_CHAINS {
                    return Box::default();
                }
                continue;
            }
            path.push(id);
            levels.push((edges.succ(id as usize), 0));
        }
        let lone = paths.len() == 1;
        paths
            .into_iter()
            .map(|p| self.chain_of(Some(p), lone))
            .collect::<Option<_>>()
            .unwrap_or_default()
    }

    /// The chain of a path (`None`: the lone path of a brace-free
    /// pattern, whose nodes are `0..elems.len()`); `None` when it has
    /// several globstars and is the graph's `lone` path (the graph's
    /// NFA serves) or would not fit a `u64`.
    fn chain_of(&self, path: Option<Vec<u32>>, lone: bool) -> Option<Chain> {
        let n = path.as_ref().map_or(self.elems.len(), Vec::len);
        let elem = |i: usize| &self.elems[path.as_ref().map_or(i, |p| p[i] as usize)];
        let mut globstars = (0..n).filter(|&i| elem(i).is_globstar());
        // The first index from which every element passes `ok`.
        let from = |ok: fn(&Elem) -> bool| {
            (0..n).rev().find(|&j| !ok(elem(j))).map_or(0, |j| j + 1) as u32
        };
        let shape = match (globstars.next(), globstars.next()) {
            // A union member gets an NFA over its nodes alone.
            (Some(_), Some(_)) => {
                if lone || n + 1 > 64 {
                    return None;
                }
                let p = path.as_deref();
                Shape::Many(Box::new(dense_nfa::<u64>(&self.elems, None, p)))
            }
            // `match_dir` reads `open_from` and `any_from` off the lone
            // chain only.
            (g, _) => Shape::Line(Line {
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

/// A chain's elements in order: the matcher's slice for the lone path
/// of a brace-free pattern, a node list over it otherwise.
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
    /// Does one of the chains match the whole path? Each is tried after
    /// two cheap looks at its ends. Out of line: `is_match` stays small
    /// for the one-chain and NFA matchers.
    #[inline(never)]
    pub(super) fn any_chain_matches(&self, path: &[u8]) -> bool {
        (0..self.chains.len()).any(|i| {
            self.ends_like(i, path) && self.starts_like(i, path) && self.chain_matches(i, path)
        })
    }

    /// Does the path end like the chain's last element: a literal, an
    /// affix's suffix (or, without one, its prefix at the last segment's
    /// start), a set's last bytes? Rejects most members of a union
    /// before the chain runs; a single chain has the prefilter for that.
    #[inline]
    pub(super) fn ends_like(&self, i: usize, path: &[u8]) -> bool {
        let chain = &self.chains[i];
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
                    let start = seg_start(path, path.len());
                    path.len() - start >= prefix.len()
                        && self.affix_eq(prefix, &path[start..start + prefix.len()])
                }
            },
            WildKind::AffixSet(set) => set.last.may_end(path),
            WildKind::AffixIndex(index) => index.last.may_end(path),
            WildKind::Generic(_) => true,
        }
    }

    #[inline]
    fn tail_is(&self, path: &[u8], suffix: &[u8]) -> bool {
        path.len() >= suffix.len() && self.affix_eq(suffix, &path[path.len() - suffix.len()..])
    }

    /// Does the path start like the chain's first element, when that is
    /// a literal: the literal, then a separator or the end?
    #[inline]
    pub(super) fn starts_like(&self, i: usize, path: &[u8]) -> bool {
        let first = self.chains[i].nodes.first().map_or(0, |&n| n as usize);
        let Elem::Lit(lit) = &self.elems[first] else {
            return true;
        };
        match path.get(lit.len()) {
            Some(&b) if !std::path::is_separator(b as char) => false,
            Some(_) => self.affix_eq(lit, &path[..lit.len()]),
            None => path.len() == lit.len() && self.affix_eq(lit, path),
        }
    }

    /// Does chain `i` match the whole path?
    pub(super) fn chain_matches(&self, i: usize, path: &[u8]) -> bool {
        let chain = &self.chains[i];
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
            Shape::Many(nfa) => nfa.is_match(self, path),
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
        let n = elems.len();
        // Where the tail starts; past the end when there is none.
        let mut split = path.len() + 1;
        let mut end = path.len();
        for j in (g + 1..n).rev() {
            let s = seg_start(path, end);
            if s < mid_start || !self.elem_consumes(elems.at(j), &path[s..end]) {
                return false;
            }
            split = s;
            if j > g + 1 {
                if s == 0 {
                    return false;
                }
                end = s - 1;
            }
        }
        self.absorbs(elems.at(g), path, mid_start, split, None)
    }

    /// Can the globstar `kind` take the segments between the head
    /// (ending before `from`) and the tail starting at `split`?
    /// `split == from` means no segment at all. `dot_at`, when given, is
    /// `first_dot(path, from)`, which makes the dot check one compare.
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
        match dot_at {
            Some(at) => at >= to,
            None => self.dot_free(path, from, to),
        }
    }

    /// Under `dot=false`, where the first segment of `path[from..]` that
    /// starts with a `.` starts; `usize::MAX` when none does.
    fn first_dot(&self, path: &[u8], from: usize) -> usize {
        if self.dot {
            return usize::MAX;
        }
        if path.get(from) == Some(&b'.') {
            return from;
        }
        (from..path.len())
            .find(|&i| std::path::is_separator(path[i] as char) && path.get(i + 1) == Some(&b'.'))
            .map_or(usize::MAX, |i| i + 1)
    }

    /// Under `dot=false`, no segment within `path[from..to)` may start
    /// with a `.`.
    fn dot_free(&self, path: &[u8], from: usize, to: usize) -> bool {
        if self.dot {
            return true;
        }
        if from < to && path[from] == b'.' {
            return false;
        }
        !(from..to)
            .any(|i| std::path::is_separator(path[i] as char) && i + 1 < to && path[i + 1] == b'.')
    }

    /// `match_dir` through the lone chain of a matcher that keeps no
    /// NFA.
    pub(super) fn chain_dir(&self, dir: &[u8]) -> DirMatch {
        let chain = &self.chains[0];
        let Shape::Line(line) = chain.shape else {
            unreachable!("a chain with several globstars keeps the NFA")
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
        let dot_at = Some(self.first_dot(dir, mid_start));
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

    #[inline(always)]
    fn elem_consumes(&self, e: &Elem, seg: &[u8]) -> bool {
        match e {
            Elem::Lit(lit) => self.lit_eq(lit, seg),
            Elem::Wild(w) => self.wild_consumes(w, seg),
            _ => false,
        }
    }
}
