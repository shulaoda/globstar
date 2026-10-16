//! Ops → segment graph → chains and NFA.
//!
//! The walk keeps one *group* per distinct (boundary, partial segment)
//! reaching a point of the pattern, so a brace whose branches end at
//! a `/` costs one node per branch and the continuation is shared;
//! nothing is multiplied out. Branches that start alike share their
//! nodes too (a node is reused when its element and predecessors
//! already exist), which turns a union of names into a trie.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::engine::ops::Op;

use super::bits::{Bits, Wide};
use super::seg_nfa::SegNfa;
use super::{
    Affix, AffixIndex, AffixSet, AnyNfa, ByteSketch, Bytes, Chain, Elem, LINEAR_SUFFIXES, LitSet,
    LitSets, MAX_CHAINS, Nfa, SegmentMatcher, Shape, State, SuffixRun, Wild, WildKind, facts,
};

/// Successor marker for "the pattern ends here".
const ACCEPT: u32 = u32::MAX;
/// Predecessor marker for the graph start.
const START: u32 = u32::MAX;

pub(super) fn compile(ops: &[Op], dot: bool, ci: bool) -> SegmentMatcher {
    let graph = Graph::build(ops, dot, ci);
    let chains = graph.chains();
    // A lone chain with at most one globstar serves `match_dir` too;
    // anything else needs the NFA.
    let nfa = match &chains[..] {
        [c] if !matches!(c.shape, Shape::Many(_)) => None,
        _ => Some(Box::new(graph.nfa())),
    };
    let suffixes = facts::suffixes(ops);
    SegmentMatcher {
        suffix_last: ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci),
        suffixes,
        chains,
        nfa,
        elems: graph.elems.into_boxed_slice(),
        prefixes: facts::static_prefixes(ops),
        case_insensitive: ci,
        dot,
    }
}

/// The graph as the walk leaves it: the nodes and, for a pattern with
/// a separator-crossing brace, their edges. A brace-free pattern is
/// one path, emitted in order; its edges are implied and never stored.
struct Graph {
    elems: Vec<Elem>,
    edges: Option<Edges>,
    ci: bool,
}

/// Successor lists in CSR form, and the start's successors.
struct Edges {
    offsets: Vec<u32>,
    targets: Vec<u32>,
    start: Vec<u32>,
}

impl Edges {
    fn succ(&self, i: usize) -> &[u32] {
        &self.targets[self.offsets[i] as usize..self.offsets[i + 1] as usize]
    }
}

impl Graph {
    fn build(ops: &[Op], dot: bool, ci: bool) -> Self {
        let (boundaries, linear) = survey(ops);
        // Only a separator-crossing brace can need regrouping.
        let ops = if linear {
            Cow::Borrowed(ops)
        } else {
            choices(ops)
        };
        let mut b = Builder::new(boundaries, linear, dot, ci);
        let groups = b.walk(&ops, vec![Group::first()]);
        for mut g in groups {
            b.settle(&mut g);
            if g.state == Boundary::Open {
                for &t in &g.tails {
                    b.end_after(t);
                }
            } else {
                let id = b.emit(b.close(&g.buf), &g.tails);
                b.end_after(id);
            }
        }
        b.finish()
    }

    /// Every path through the graph as a `Chain`, in pattern order;
    /// none when there are more than `MAX_CHAINS` or a path with
    /// several globstars has more than 64 states (the NFA takes
    /// those).
    fn chains(&self) -> Box<[Chain]> {
        let Some(edges) = &self.edges else {
            return self.chain(None).into_iter().collect();
        };
        let mut paths: Vec<Vec<u32>> = Vec::new();
        // Depth-first, first branch first, along one path: a level per
        // node on it (the start's below them) and the successor it tries
        // next. A path is copied only when it reaches the end.
        let mut path: Vec<u32> = Vec::new();
        let mut levels: Vec<(&[u32], usize)> = vec![(&edges.start, 0)];
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
        paths
            .into_iter()
            .map(|p| self.chain(Some(p)))
            .collect::<Option<_>>()
            .unwrap_or_default()
    }

    /// The chain of a path (`None`: the lone path of a brace-free
    /// pattern, whose nodes are `0..elems.len()`); `None` when its
    /// several-globstar NFA would not fit a `u64`.
    fn chain(&self, path: Option<Vec<u32>>) -> Option<Chain> {
        let n = path.as_ref().map_or(self.elems.len(), Vec::len);
        let elem = |i: usize| &self.elems[path.as_ref().map_or(i, |p| p[i] as usize)];
        let mut globstars = (0..n).filter(|&i| elem(i).is_globstar());
        let shape = match (globstars.next(), globstars.next()) {
            (None, _) => Shape::Fixed,
            (Some(g), None) => Shape::Single(g as u32),
            // The lone path of a linear pattern has the matcher's own
            // NFA; any other path gets one over its nodes alone.
            (Some(_), Some(_)) => Shape::Many(match &path {
                None => None,
                Some(p) => {
                    if n_states((0..n).map(elem)) > 64 {
                        return None;
                    }
                    Some(Box::new(nfa_over::<u64>(
                        &self.elems,
                        self.ci,
                        None,
                        Some(p),
                    )))
                }
            }),
        };
        Some(Chain {
            nodes: path.map_or_else(Default::default, Vec::into_boxed_slice),
            shape,
        })
    }

    /// The NFA over the whole graph: a `u64` active set when it fits.
    fn nfa(&self) -> AnyNfa {
        if n_states(self.elems.iter()) <= 64 {
            AnyNfa::Small(nfa_over(&self.elems, self.ci, self.edges.as_ref(), None))
        } else {
            AnyNfa::Wide(nfa_over(&self.elems, self.ci, self.edges.as_ref(), None))
        }
    }
}

/// The NFA over the positions of a path (`None`: the graph itself,
/// whose `edges` a brace pattern stores; a linear one has none).
/// States are numbered by position, the accept state last. On the
/// graph of a brace pattern, literal siblings (same predecessor) are
/// gathered into `LitSet`s.
fn nfa_over<B: Bits>(
    elems: &[Elem],
    ci: bool,
    edges: Option<&Edges>,
    path: Option<&[u32]>,
) -> Nfa<B> {
    let n = path.map_or(elems.len(), <[u32]>::len);
    let node_at = |i: usize| path.map_or(i as u32, |p| p[i]);
    let elem = |i: usize| &elems[node_at(i) as usize];
    // Positions of a path follow one another.
    let edges = if path.is_some() { None } else { edges };
    let succs = |i: usize| {
        let (next, stored): (Option<u32>, &[u32]) = match edges {
            Some(e) => (None, e.succ(i)),
            None => (Some(if i + 1 < n { i as u32 + 1 } else { ACCEPT }), &[]),
        };
        next.into_iter().chain(stored.iter().copied())
    };
    let start: &[u32] = edges.map_or(&[0], |e| &e.start);

    // Entry state per position; on the stack for the common sizes.
    let mut stack = [0u32; 64];
    let mut heap = Vec::new();
    let entries: &mut [u32] = if n <= stack.len() {
        &mut stack[..n]
    } else {
        heap.resize(n, 0);
        &mut heap
    };
    let mut n_states = 0usize;
    for (i, slot) in entries.iter_mut().enumerate() {
        *slot = n_states as u32;
        n_states += elem(i).states();
    }
    let accept = n_states;
    n_states += 1;
    let words = B::words(n_states);

    let mut states: Vec<State<B>> = (0..n_states)
        .map(|_| State {
            after: B::zero(words),
            node: 0,
        })
        .collect();
    let mut reach1 = B::zero(words);

    // The closure some targets give, from the last position back:
    // successors always lie ahead, so each is complete when needed,
    // and a nullable successor contributes its own successors — its
    // `after` minus its own states.
    let closure_of = |targets: &mut dyn Iterator<Item = u32>, states: &[State<B>]| {
        let mut c = B::zero(words);
        for t in targets {
            if t == ACCEPT {
                c.set(accept);
                continue;
            }
            let t = t as usize;
            let e = entries[t] as usize;
            let te = elem(t);
            if te.nullable() {
                c.or_with(&states[e].after);
                if te.states() == 2 {
                    c.unset(e + 1);
                }
            }
            c.set(e);
        }
        c
    };
    for i in (0..n).rev() {
        let e = entries[i] as usize;
        let el = elem(i);
        states[e].node = node_at(i);
        let mut c = closure_of(&mut succs(i), &states);
        // A state reaches accept through ≥ 1 segment iff its element
        // can consume one and what follows reaches accept.
        if el.satisfiable() && (c.test(accept) || c.intersects(&reach1)) {
            reach1.set(e);
            if el.states() == 2 {
                reach1.set(e + 1);
            }
        }
        match el {
            Elem::Lit(_) | Elem::Wild(_) => states[e].after = c,
            Elem::G0 => {
                c.set(e);
                states[e].after = c;
            }
            Elem::G0Strict | Elem::G1 => {
                states[e + 1].node = node_at(i);
                c.set(e + 1);
                states[e].after = c.clone();
                states[e + 1].after = c;
            }
        }
    }
    let init = closure_of(&mut start.iter().copied(), &states);

    // Literal siblings: the `Lit` successors of one position (or of
    // the start) when there are at least two.
    let mut lit_sets: Vec<LitSet<B>> = Vec::new();
    let mut grouped = B::zero(words);
    if edges.is_some() {
        let mut lit_set = |targets: &mut dyn Iterator<Item = u32>| {
            let mut positions: Vec<u32> = targets
                .filter(|&t| t != ACCEPT && matches!(elem(t as usize), Elem::Lit(_)))
                .collect();
            if positions.len() < 2 {
                return;
            }
            sort_by_literal(&mut positions, elems, path, ci);
            let sorted: Box<[u32]> = positions.iter().map(|&t| entries[t as usize]).collect();
            let mut set = B::zero(words);
            for &s in &sorted {
                set.set(s as usize);
            }
            grouped.or_with(&set);
            lit_sets.push(LitSet {
                states: set,
                sorted,
            });
        };
        lit_set(&mut start.iter().copied());
        for i in 0..n {
            lit_set(&mut succs(i));
        }
    }

    Nfa {
        states: states.into_boxed_slice(),
        init,
        reach1,
        lit_sets: (!lit_sets.is_empty()).then(|| {
            Box::new(LitSets {
                sets: lit_sets.into_boxed_slice(),
                states: grouped,
            })
        }),
    }
}

/// NFA states over `elems`, the accept state included.
fn n_states<'a>(elems: impl Iterator<Item = &'a Elem>) -> usize {
    elems.map(Elem::states).sum::<usize>() + 1
}

/// Positions holding literals, by literal (case-folded when
/// insensitive) then position, duplicates dropped. Not generic over the
/// bit set, so the sort is compiled once.
fn sort_by_literal(positions: &mut Vec<u32>, elems: &[Elem], path: Option<&[u32]>, ci: bool) {
    let lit = |t: u32| elems[path.map_or(t as usize, |p| p[t as usize] as usize)].as_lit();
    positions.sort_unstable_by(|&a, &b| lit_cmp(lit(a), lit(b), ci).then(a.cmp(&b)));
    positions.dedup();
}

/// Byte order, ASCII case folded when insensitive.
pub(super) fn lit_cmp(a: &[u8], b: &[u8], ci: bool) -> std::cmp::Ordering {
    if !ci {
        return a.cmp(b);
    }
    a.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(b.iter().map(u8::to_ascii_lowercase))
}

/// The NFA over the lone chain a matcher kept instead of one (test
/// hook).
pub(super) fn chain_nfa(m: &SegmentMatcher) -> Nfa<Wide> {
    let nodes = &m.chains[0].nodes;
    let path = (!nodes.is_empty()).then_some(&nodes[..]);
    nfa_over(&m.elems, m.case_insensitive, None, path)
}

/// Elements a brace-free pattern needs (one per boundary op plus the
/// last segment; with braces a floor), and whether the pattern has a
/// separator-crossing brace at all.
fn survey(ops: &[Op]) -> (usize, bool) {
    let mut boundaries = 0;
    let mut linear = true;
    for op in ops {
        match op {
            Op::Alternation(branches) => {
                if crosses(op) {
                    linear = false;
                }
                for b in branches {
                    boundaries += survey(b).0;
                }
            }
            Op::Lit(_) | Op::AnyChar | Op::Star | Op::Class(_) | Op::LeadingSeps => {}
            _ => boundaries += 1,
        }
    }
    (boundaries, linear)
}

/// What the last op left behind for the next element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    /// Pattern start.
    Start,
    /// Right after a globstar element.
    AfterG,
    /// Just consumed a `Sep`.
    Strict,
    /// Just consumed a `SepRun` or `LeadingSeps` (lenient `**` boundary).
    Lenient,
    /// A bare `**` absorbs the rest of the path.
    Open,
}

/// Paths under construction that agree on everything still to come:
/// the nodes awaiting a successor (`START` for the graph start), the
/// boundary just crossed and the in-segment ops read since.
#[derive(Clone)]
struct Group<'a> {
    tails: Vec<u32>,
    state: Boundary,
    buf: Vec<&'a Op>,
    /// A `**/` read but not yet emitted: a bare `**` right after it
    /// absorbs it (`x/**/**` ≡ `x/**`), anything else makes it real.
    /// More of them add nothing (`**/**/` ≡ `**/`). While set, `state`
    /// is the boundary before it and `buf` is empty.
    pending_oss: bool,
}

impl Group<'_> {
    fn first() -> Self {
        Self {
            tails: vec![START],
            state: Boundary::Start,
            buf: Vec::new(),
            pending_oss: false,
        }
    }

    /// Drop repeated tails, which merges and reused nodes add; their
    /// order means nothing (successors are listed by node).
    fn dedupe_tails(&mut self) {
        if self.tails.len() > 1 {
            self.tails.sort_unstable();
            self.tails.dedup();
        }
    }

    /// Continue as the one group after `id`, reusing the vectors. The
    /// partial segment stays: a pending `**/` settles ahead of it.
    fn rebase(&mut self, id: u32, state: Boundary) {
        self.tails.clear();
        self.tails.push(id);
        self.state = state;
        self.pending_oss = false;
    }
}

/// Builds the graph. A pattern with a separator-crossing brace also
/// records every node's predecessors (flat, `START` for the start,
/// node `i` owning `preds[pred_at[i]..pred_at[i + 1]]`), the nodes the
/// pattern may end after, and reuses a node by its (element,
/// predecessors) hash; a linear pattern records its nodes only.
struct Builder {
    elems: Vec<Elem>,
    linear: bool,
    preds: Vec<u32>,
    pred_at: Vec<u32>,
    ends: Vec<u32>,
    memo: HashMap<u64, u32, BuildHasherDefault<Fnv>>,
    dot: bool,
    ci: bool,
}

impl Builder {
    fn new(boundaries: usize, linear: bool, dot: bool, ci: bool) -> Self {
        Builder {
            elems: Vec::with_capacity(boundaries + 1),
            linear,
            preds: Vec::new(),
            pred_at: Vec::new(),
            ends: Vec::new(),
            memo: HashMap::default(),
            dot,
            ci,
        }
    }

    fn finish(self) -> Graph {
        let edges = (!self.linear).then(|| self.successors());
        Graph {
            elems: self.elems,
            edges,
            ci: self.ci,
        }
    }

    fn preds_of(&self, id: usize) -> &[u32] {
        let start = self.pred_at[id] as usize;
        let end = self
            .pred_at
            .get(id + 1)
            .map_or(self.preds.len(), |&e| e as usize);
        &self.preds[start..end]
    }

    /// Invert the predecessor lists into CSR successor lists.
    fn successors(&self) -> Edges {
        let n = self.elems.len();
        let mut offsets = vec![0u32; n + 1];
        for &from in self.preds.iter().chain(&self.ends) {
            if from != START {
                offsets[from as usize + 1] += 1;
            }
        }
        for i in 0..n {
            offsets[i + 1] += offsets[i];
        }
        let mut fill = offsets.clone();
        let mut targets = vec![0u32; offsets[n] as usize];
        let mut start = Vec::new();
        for id in 0..n {
            for &from in self.preds_of(id) {
                if from == START {
                    start.push(id as u32);
                    continue;
                }
                let slot = &mut fill[from as usize];
                targets[*slot as usize] = id as u32;
                *slot += 1;
            }
        }
        for &from in &self.ends {
            if from != START {
                let slot = &mut fill[from as usize];
                targets[*slot as usize] = ACCEPT;
                *slot += 1;
            }
        }
        Edges {
            offsets,
            targets,
            start,
        }
    }

    /// A node for `elem` after `tails`; an existing one when a branch
    /// already made it.
    fn emit(&mut self, elem: Elem, tails: &[u32]) -> u32 {
        let id = self.elems.len() as u32;
        if self.linear {
            self.elems.push(elem);
            return id;
        }
        let key = self.key(&elem, tails);
        if let Some(&found) = key.and_then(|k| self.memo.get(&k))
            && self.same(found, &elem, tails)
        {
            return found;
        }
        self.elems.push(elem);
        self.pred_at.push(self.preds.len() as u32);
        self.preds.extend_from_slice(tails);
        if let Some(k) = key {
            self.memo.entry(k).or_insert(id);
        }
        id
    }

    /// Reuse key of a literal or globstar node; wildcards stay apart.
    fn key(&self, elem: &Elem, tails: &[u32]) -> Option<u64> {
        let mut h = Fnv::default();
        match elem {
            Elem::Lit(lit) => {
                h.write(&[0]);
                h.write(lit);
            }
            Elem::G0 => h.write(&[1]),
            Elem::G0Strict => h.write(&[2]),
            Elem::G1 => h.write(&[3]),
            Elem::Wild(_) => return None,
        }
        for &t in tails {
            h.write(&t.to_le_bytes());
        }
        Some(h.finish())
    }

    /// Is node `id` this element after these predecessors?
    fn same(&self, id: u32, elem: &Elem, tails: &[u32]) -> bool {
        let i = id as usize;
        if self.preds_of(i) != tails {
            return false;
        }
        match (&self.elems[i], elem) {
            (Elem::Lit(a), Elem::Lit(b)) => a == b,
            (Elem::G0, Elem::G0) | (Elem::G0Strict, Elem::G0Strict) | (Elem::G1, Elem::G1) => true,
            _ => false,
        }
    }

    /// Record that the pattern may end after `from`.
    fn end_after(&mut self, from: u32) {
        if !self.linear {
            self.ends.push(from);
        }
    }

    fn walk<'a>(&mut self, ops: &'a [Op], mut groups: Vec<Group<'a>>) -> Vec<Group<'a>> {
        for op in ops {
            // Lowering never puts an op after an open globstar; should
            // one appear, the open paths absorb the rest of the path.
            debug_assert!(groups.iter().all(|g| g.state != Boundary::Open));
            // Ops that may still be absorbed by a bare `**` keep the
            // pending `**/` elements; anything else makes them real.
            if !matches!(
                op,
                Op::GlobstarAny | Op::OptSegmentsSlash | Op::LeadingSeps | Op::Alternation(_)
            ) {
                for g in groups.iter_mut() {
                    self.settle(g);
                }
            }
            match op {
                Op::Alternation(branches) if crosses(op) => {
                    let mut out: Vec<Group<'a>> = Vec::new();
                    for branch in branches {
                        let copies = groups.to_vec();
                        for g in self.walk(branch, copies) {
                            merge(&mut out, g);
                        }
                    }
                    for g in &mut out {
                        g.dedupe_tails();
                    }
                    groups = out;
                }
                Op::Sep => self.close_all(&mut groups, Boundary::Strict),
                Op::SepRun => self.close_all(&mut groups, Boundary::Lenient),
                Op::LeadingSeps => {
                    // `/*`: a strict boundary becomes lenient for the
                    // `**/` that follows; after a `**/` it adds nothing.
                    for g in groups
                        .iter_mut()
                        .filter(|g| g.state == Boundary::Strict && !g.pending_oss)
                    {
                        g.state = Boundary::Lenient;
                    }
                }
                Op::OptSegmentsSlash => {
                    for g in groups.iter_mut().filter(|g| g.state != Boundary::Open) {
                        // The parser never glues a `**` to text.
                        debug_assert!(g.buf.is_empty());
                        g.pending_oss = true;
                    }
                }
                Op::SlashAnything => {
                    self.close_all(&mut groups, Boundary::Strict);
                    self.globstar_all(&mut groups, |_| Elem::G1);
                }
                Op::GlobstarAny => {
                    // The bare `**` owns the rest of the path: pending
                    // `**/` add nothing to it, and a separator before
                    // them still needs a segment to follow (`x/**/**`
                    // ≡ `x/**`), so only the pattern start gives `G0`.
                    for g in groups.iter_mut() {
                        g.pending_oss = false;
                    }
                    self.globstar_all(&mut groups, |state| {
                        if state == Boundary::Start {
                            Elem::G0
                        } else {
                            Elem::G1
                        }
                    });
                }
                Op::Globstar => unreachable!("raw `**` survives lowering"),
                op => {
                    for g in groups.iter_mut().filter(|g| g.state != Boundary::Open) {
                        g.buf.push(op);
                    }
                }
            }
        }
        groups
    }

    /// Emit a group's pending `**/`, its kind by the boundary before it.
    fn settle(&mut self, g: &mut Group<'_>) {
        if !g.pending_oss || g.state == Boundary::Open {
            return;
        }
        let kind = match g.state {
            Boundary::Start | Boundary::Lenient => Elem::G0,
            _ => Elem::G0Strict,
        };
        let id = self.emit(kind, &g.tails);
        g.rebase(id, Boundary::AfterG);
    }

    /// Close every group's segment into one node per group; they all
    /// continue as one group at the new boundary (the first group's
    /// vectors carry on, in the same list).
    fn close_all<'a>(&mut self, groups: &mut Vec<Group<'a>>, state: Boundary) {
        self.join_all(groups, state, |b, g| {
            b.settle(g);
            let elem = b.close(&g.buf);
            g.buf.clear();
            b.emit(elem, &g.tails)
        });
    }

    /// One globstar node per group, its kind chosen by the boundary
    /// the group crossed; they continue as one open group.
    fn globstar_all<'a>(&mut self, groups: &mut Vec<Group<'a>>, kind: impl Fn(Boundary) -> Elem) {
        self.join_all(groups, Boundary::Open, |b, g| {
            // The parser never glues a `**` to text.
            debug_assert!(g.buf.is_empty());
            b.emit(kind(g.state), &g.tails)
        });
    }

    /// Emit one node per group through `node`, then continue as one
    /// group at `state`. Open groups pass through untouched.
    fn join_all<'a>(
        &mut self,
        groups: &mut Vec<Group<'a>>,
        state: Boundary,
        node: impl Fn(&mut Self, &mut Group<'a>) -> u32,
    ) {
        let mut joined: Option<Group<'a>> = None;
        let mut open: Vec<Group<'a>> = Vec::new();
        for mut g in groups.drain(..) {
            if g.state == Boundary::Open {
                merge(&mut open, g);
                continue;
            }
            let id = node(self, &mut g);
            match &mut joined {
                Some(j) => j.tails.push(id),
                None => {
                    g.rebase(id, state);
                    joined = Some(g);
                }
            }
        }
        groups.extend(open);
        groups.extend(joined);
        for g in groups.iter_mut() {
            g.dedupe_tails();
        }
    }

    fn close(&self, buf: &[&Op]) -> Elem {
        if !buf.iter().all(|op| matches!(op, Op::Lit(_))) {
            return Elem::Wild(compile_wild(buf, self.dot, self.ci));
        }
        // Empty, one literal, or literals glued across a brace edge.
        let lit = concat_lits(buf);
        if lit.iter().any(|&b| std::path::is_separator(b as char)) {
            // An escaped `\` on Windows: no segment holds a separator, so
            // the literal is a silent miss (§2.4), kept as a wildcard
            // that wants more bytes than any segment has.
            return Elem::Wild(Wild {
                kind: WildKind::Affix(Affix::new(lit, Bytes::default())),
                min_len: u32::MAX,
                variable: false,
                dot_protect: false,
                any_seg: false,
            });
        }
        Elem::Lit(lit)
    }
}

/// Add a group to `out`, joining one that agrees on boundary and
/// partial segment: their continuations are the same nodes.
fn merge<'a>(out: &mut Vec<Group<'a>>, g: Group<'a>) {
    if let Some(same) = out.iter_mut().find(|o| {
        o.state == g.state
            && o.pending_oss == g.pending_oss
            && o.buf.len() == g.buf.len()
            && o.buf
                .iter()
                .zip(&g.buf)
                .all(|(a, b)| std::ptr::eq(*a, *b) || a == b)
    }) {
        same.tails.extend(g.tails);
    } else {
        out.push(g);
    }
}

/// The ops with each crossing brace's in-segment branches as one choice:
/// `{a,b,/}` walks as `{{a,b},/}`. Forked one by one, such branches give
/// a group per partial segment, 2^k of them after k braces.
fn choices(ops: &[Op]) -> Cow<'_, [Op]> {
    if !ops.iter().any(splits_choices) {
        return Cow::Borrowed(ops);
    }
    let regroup = |op: &Op| {
        let Op::Alternation(branches) = op else {
            return op.clone();
        };
        if !crosses(op) {
            return op.clone();
        }
        let mut out: Vec<Vec<Op>> = Vec::with_capacity(branches.len());
        let mut within: Vec<Vec<Op>> = Vec::new();
        let mut at = None;
        for b in branches {
            if b.iter().any(crosses) {
                out.push(choices(b).into_owned());
            } else {
                at.get_or_insert(out.len());
                within.push(b.clone());
            }
        }
        if let Some(at) = at {
            let choice = match within.len() {
                1 => within.pop().unwrap(),
                _ => vec![Op::Alternation(within)],
            };
            out.insert(at, choice);
        }
        Op::Alternation(out)
    };
    Cow::Owned(ops.iter().map(regroup).collect())
}

/// Does a crossing brace in `op` have two or more in-segment branches?
fn splits_choices(op: &Op) -> bool {
    let Op::Alternation(branches) = op else {
        return false;
    };
    crosses(op)
        && (branches.iter().filter(|b| !b.iter().any(crosses)).count() > 1
            || branches.iter().any(|b| b.iter().any(splits_choices)))
}

/// Does the op hold a segment boundary?
fn crosses(op: &Op) -> bool {
    match op {
        Op::Lit(_) | Op::AnyChar | Op::Star | Op::Class(_) => false,
        Op::Alternation(branches) => branches.iter().any(|b| b.iter().any(crosses)),
        _ => true,
    }
}

/// FNV-1a: the node keys are a few bytes, hashed once per node.
#[derive(Default)]
struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for &b in bytes {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = h;
    }
}

/// The bytes of a run of `Lit` ops, allocated once.
fn concat_lits(ops: &[&Op]) -> Bytes {
    fn lit(op: &Op) -> &[u8] {
        match op {
            Op::Lit(bytes) => bytes,
            _ => unreachable!("not a literal"),
        }
    }
    match ops {
        [one] => Bytes::from(lit(one)),
        _ => {
            let mut acc = Vec::with_capacity(ops.iter().map(|op| lit(op).len()).sum());
            for op in ops {
                acc.extend_from_slice(lit(op));
            }
            acc.into_boxed_slice()
        }
    }
}

fn compile_wild(ops: &[&Op], dot: bool, ci: bool) -> Wild {
    let mut idx = ops.iter().take_while(|op| matches!(op, Op::Lit(_))).count();
    let prefix = concat_lits(&ops[..idx]);
    let mut anychars = 0u32;
    let mut has_star = false;
    while idx < ops.len() {
        match ops[idx] {
            Op::Star => has_star = true,
            Op::AnyChar => anychars += 1,
            _ => break,
        }
        idx += 1;
    }
    let has_wilds = has_star || anychars > 0;
    let dot_protect = !dot && prefix.is_empty() && has_wilds;
    // `*` or `?*` with nothing literal around it, up to the suffix.
    let any_head = prefix.is_empty() && has_star && anychars <= 1;

    if idx == ops.len() {
        return Wild {
            min_len: prefix.len() as u32 + anychars,
            kind: WildKind::Affix(Affix::new(prefix, Bytes::default())),
            variable: has_star,
            dot_protect,
            any_seg: any_head,
        };
    }
    if let Some(suffixes) = suffix_product(&ops[idx..]) {
        let any_seg = any_head && suffixes.iter().any(|s| s.is_empty());
        if let [suffix] = &suffixes[..] {
            return Wild {
                min_len: (prefix.len() + suffix.len()) as u32 + anychars,
                kind: WildKind::Affix(Affix::new(prefix, Bytes::from(&suffix[..]))),
                variable: has_star,
                dot_protect,
                any_seg,
            };
        }
        return Wild {
            min_len: prefix.len() as u32 + anychars,
            kind: affix_set(prefix, suffixes, ci),
            variable: has_star,
            dot_protect,
            any_seg,
        };
    }
    let nfa = SegNfa::compile(ops, dot, ci);
    Wild {
        dot_protect: !dot && nfa.wild_led,
        any_seg: nfa.any_seg,
        kind: WildKind::Generic(Box::new(nfa)),
        min_len: 0,
        variable: true,
    }
}

/// Literal suffix alternatives one segment enumerates (`*.{a,b,…}`, a
/// factored union of names). A product of alternatives (`{a,b}{c,d}…`)
/// past this many suffixes stays an NFA, which holds it without
/// multiplying it out; a single list is no product and is enumerated
/// whatever its length.
const MAX_SUFFIX_PRODUCT: usize = 1024;
/// The enumerated suffixes, one byte more each, may not take more than
/// this or four times their ops: a long literal next to a product would
/// be copied into every suffix.
const MAX_SUFFIX_BYTES: usize = 64 * 1024;

fn suffix_product(ops: &[&Op]) -> Option<Vec<Vec<u8>>> {
    if let [Op::Lit(bytes)] = ops {
        return Some(vec![bytes.clone()]);
    }
    let mut parts: Vec<Vec<u8>> = vec![Vec::new()];
    // Σ (part + 1) bytes, and what the ops themselves hold.
    let (mut size, mut source) = (1usize, 1usize);
    let fits = |size: usize, source: usize| size <= MAX_SUFFIX_BYTES.max(source.saturating_mul(4));
    for op in ops {
        match op {
            Op::Lit(bytes) => {
                size = size.saturating_add(parts.len().saturating_mul(bytes.len()));
                source += bytes.len();
                if !fits(size, source) {
                    return None;
                }
                for p in parts.iter_mut() {
                    p.extend_from_slice(bytes);
                }
            }
            Op::Alternation(branches) => {
                let mut lits = Vec::with_capacity(branches.len());
                for b in branches {
                    match b.as_slice() {
                        [] => lits.push(&b""[..]),
                        [Op::Lit(bytes)] => lits.push(bytes.as_slice()),
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

/// Up to `LINEAR_SUFFIXES` the set is scanned in pattern order; past
/// it the suffixes are sorted by (length, bytes) and indexed per
/// length so `wild_consumes` can binary-search them.
fn affix_set(prefix: Bytes, suffixes: Vec<Vec<u8>>, ci: bool) -> WildKind {
    // The product's buffers are exact, so this keeps them as they are.
    let mut suffixes: Vec<Bytes> = suffixes.into_iter().map(Vec::into_boxed_slice).collect();
    let last = ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci);
    if suffixes.len() <= LINEAR_SUFFIXES {
        return WildKind::AffixSet(AffixSet {
            parts: std::iter::once(prefix).chain(suffixes).collect(),
            last,
        });
    }
    if ci {
        for s in &mut suffixes {
            s.make_ascii_lowercase();
        }
    }
    suffixes.sort_unstable_by(len_then_bytes);
    suffixes.dedup();
    let mut bytes = Vec::with_capacity(suffixes.iter().map(|s| s.len()).sum());
    let mut runs: Vec<SuffixRun> = Vec::new();
    for s in &suffixes {
        match runs.last_mut() {
            Some(run) if run.len as usize == s.len() => run.count += 1,
            _ => runs.push(SuffixRun {
                len: s.len() as u32,
                start: bytes.len() as u32,
                count: 1,
            }),
        }
        bytes.extend_from_slice(s);
    }
    WildKind::AffixIndex(Box::new(AffixIndex {
        prefix,
        suffixes: bytes.into_boxed_slice(),
        last,
        runs: runs.into_boxed_slice(),
    }))
}

/// (length, bytes) order. One comparator, so its sort is compiled once.
pub(super) fn len_then_bytes<T: AsRef<[u8]>>(a: &T, b: &T) -> std::cmp::Ordering {
    let (a, b) = (a.as_ref(), b.as_ref());
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}
