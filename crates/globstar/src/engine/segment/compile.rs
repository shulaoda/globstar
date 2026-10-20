//! AST → segment graph → chains and NFA.
//!
//! The walk keeps one *group* per distinct (boundary, partial segment)
//! reaching a point of the pattern, so a brace whose branches end at
//! a `/` costs one node per branch and the continuation is shared;
//! nothing is multiplied out. Branches that start alike share their
//! nodes too (a node is reused when its element and predecessors
//! already exist), which turns a union of names into a trie.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};

use crate::ast::Node;
use crate::error::GlobError;

use super::bits::{Bits, Wide};
use super::seg_nfa::SegNfa;
use super::{
    Affix, AffixIndex, AffixSet, AnyNfa, ByteSketch, Bytes, Chain, Elem, MAX_CHAINS, NEVER, Nfa,
    SegmentMatcher, Shape, SparseNfa, State, SuffixRun, Wild, WildKind, facts, has_sep,
};

/// Successor marker for "the pattern ends here".
const ACCEPT: u32 = u32::MAX;
/// Predecessor marker for the graph start.
const START: u32 = u32::MAX;

/// The work the graph walk may take: 64 units per unit of pattern
/// weight (`node_weight`), and never less than 2^20. A unit is a node,
/// an edge, a copied token, or a byte of the tables a wildcard segment
/// builds (`Wild::table_bytes`). Patterns of ordinary shape need one to
/// three per unit; only a shape whose segments multiply (`a{a,/}`
/// repeated, one node per pair of braces) reaches it, and it fails
/// before it costs much.
const BUDGET_PER_WEIGHT: usize = 64;
const MIN_BUDGET: usize = 1 << 20;

pub(super) fn compile(body: &Node, dot: bool, ci: bool) -> Result<SegmentMatcher, GlobError> {
    let graph = Graph::build(body, dot, ci)?;
    let chains = graph.chains();
    // A lone chain serves `match_dir` too; anything else needs the NFA.
    let nfa = match &chains[..] {
        [_] => None,
        _ => Some(Box::new(any_nfa(&graph.elems, ci, graph.edges.as_ref()))),
    };
    let suffixes = facts::suffixes(body);
    Ok(SegmentMatcher {
        suffix_last: ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci),
        suffixes,
        chains,
        nfa,
        elems: graph.elems.into_boxed_slice(),
        prefixes: facts::static_prefixes(body, graph.weight),
        case_insensitive: ci,
        dot,
    })
}

/// The graph as the walk leaves it: the nodes and, for a pattern with
/// a separator-crossing brace, their edges. A brace-free pattern is
/// one path, emitted in order; its edges are implied and never stored.
struct Graph {
    elems: Vec<Elem>,
    edges: Option<Edges>,
    /// The pattern's `node_weight`.
    weight: usize,
}

/// Successor lists in CSR form, the start's last (position `n`).
struct Edges {
    offsets: Vec<u32>,
    targets: Vec<u32>,
}

impl Edges {
    fn succ(&self, i: usize) -> &[u32] {
        &self.targets[self.offsets[i] as usize..self.offsets[i + 1] as usize]
    }
}

impl Graph {
    fn build(body: &Node, dot: bool, ci: bool) -> Result<Self, GlobError> {
        let nodes = body.seq();
        let (boundaries, linear, weight) = survey(nodes);
        // Only a separator-crossing brace can need regrouping.
        let nodes = if linear {
            Cow::Borrowed(nodes)
        } else {
            choices(nodes)
        };
        let mut b = Builder::new(boundaries, linear, dot, ci);
        // A linear walk keeps one group and takes a few units per weight,
        // so only a separator-crossing brace can reach the budget.
        if !linear {
            b.budget = MIN_BUDGET.max(weight.saturating_mul(BUDGET_PER_WEIGHT));
        }
        let groups = b.walk(&nodes, false, vec![Group::first()]);
        for mut g in groups {
            b.settle(&mut g);
            if g.state == Boundary::Open {
                for &t in &g.tails {
                    b.end_after(t);
                }
            } else {
                b.cost += g.weight;
                let elem = b.close(&g.buf);
                let id = b.emit(elem, &g.tails);
                b.end_after(id);
            }
        }
        if b.over() {
            return Err(GlobError::TooComplex { max: b.budget });
        }
        Ok(b.finish(weight))
    }

    /// Every path through the graph as a `Chain`, in pattern order;
    /// none when there are more than `MAX_CHAINS`, or a path with
    /// several globstars is the only path or has more than 64 states
    /// (the NFA takes those).
    fn chains(&self) -> Box<[Chain]> {
        let Some(edges) = &self.edges else {
            return self.chain(None, true).into_iter().collect();
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
            .map(|p| self.chain(Some(p), lone))
            .collect::<Option<_>>()
            .unwrap_or_default()
    }

    /// The chain of a path (`None`: the lone path of a brace-free
    /// pattern, whose nodes are `0..elems.len()`); `None` when it has
    /// several globstars and is the graph's `lone` path (the graph's
    /// NFA serves) or would not fit a `u64`.
    fn chain(&self, path: Option<Vec<u32>>, lone: bool) -> Option<Chain> {
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
                Shape::Many(Box::new(nfa_over::<u64>(&self.elems, None, p)))
            }
            // `match_dir` reads `open_from` and `any_from` off the lone
            // chain only.
            (g, _) => Shape::Line {
                g: g.unwrap_or(n) as u32,
                open_from: if lone { from(Elem::satisfiable) } else { 0 },
                any_from: if lone { from(Elem::any_seg) } else { 0 },
            },
        };
        Some(Chain {
            nodes: path.map_or_else(Default::default, Vec::into_boxed_slice),
            shape,
        })
    }
}

/// The NFA over the graph: dense rows in a `u64`, or past 64 states
/// within `MAX_ROW_BYTES` and `MAX_DENSE_LITS`; sparse beyond.
fn any_nfa(elems: &[Elem], ci: bool, edges: Option<&Edges>) -> AnyNfa {
    let states = elems.len() + 1;
    if states <= 64 {
        AnyNfa::Small(nfa_over(elems, edges, None))
    } else if states * Wide::words(states) * 8 <= MAX_ROW_BYTES
        && edges.is_none_or(|e| lit_fanout(elems, e) <= MAX_DENSE_LITS)
    {
        AnyNfa::Wide(Box::new(nfa_over(elems, edges, None)))
    } else {
        AnyNfa::Sparse(Box::new(sparse_over(elems, ci, edges)))
    }
}

/// Dense rows test each active state, so they pay off while few states
/// are active together. The sparse NFA finds a segment among a
/// position's literals by one search: it wins on a wide literal fan-out
/// (a union of 50 names) and past about 170 states, and its tables are
/// linear in the graph.
const MAX_ROW_BYTES: usize = 4 * 1024;
const MAX_DENSE_LITS: usize = 48;

/// The most literals one position (or the start) leads to.
fn lit_fanout(elems: &[Elem], edges: &Edges) -> usize {
    (0..=elems.len())
        .map(|i| {
            let succ = edges.succ(i).iter();
            succ.filter(|&&t| t != ACCEPT && matches!(elems[t as usize], Elem::Lit(_)))
                .count()
        })
        .fold(0, usize::max)
}

/// The successors of position `p` (`n`: the start): the stored ones
/// of a brace pattern's graph, else the next position.
fn succ_of(edges: Option<&Edges>, n: usize, p: usize) -> impl Iterator<Item = u32> + '_ {
    let (next, stored): (Option<u32>, &[u32]) = match edges {
        Some(e) => (None, e.succ(p)),
        None if p == n => (Some(0), &[]),
        None => (Some(if p + 1 < n { p as u32 + 1 } else { ACCEPT }), &[]),
    };
    next.into_iter().chain(stored.iter().copied())
}

/// The NFA over the positions of a path, which follow one another
/// (`None`: the graph itself, whose `edges` a brace pattern stores).
/// State `i` is position `i`, the accept state last.
fn nfa_over<B: Bits>(elems: &[Elem], edges: Option<&Edges>, path: Option<&[u32]>) -> Nfa<B> {
    let n = path.map_or(elems.len(), <[u32]>::len);
    let node_at = |i: usize| path.map_or(i as u32, |p| p[i]);
    let elem = |i: usize| &elems[node_at(i) as usize];
    let accept = n;
    let words = B::words(n + 1);
    // The rows are filled below; the accept state's is never read.
    let mut states: Vec<State<B>> = (0..=n)
        .map(|_| State {
            after: B::zero(0),
            node: 0,
        })
        .collect();
    let mut reach1 = B::zero(words);

    // The closure some targets give, from the last position back:
    // successors always lie ahead, so each is complete when needed,
    // and a nullable successor contributes its own successors.
    let closure_of = |targets: &mut dyn Iterator<Item = u32>, states: &[State<B>]| {
        let mut c = B::zero(words);
        for t in targets {
            if t == ACCEPT {
                c.set(accept);
                continue;
            }
            let t = t as usize;
            if elem(t).nullable() {
                c.or_with(&states[t].after);
            }
            c.set(t);
        }
        c
    };
    for i in (0..n).rev() {
        let el = elem(i);
        states[i].node = node_at(i);
        let mut c = closure_of(&mut succ_of(edges, n, i), &states);
        // A state reaches accept through ≥ 1 segment iff its element
        // can consume one and what follows reaches accept.
        if el.satisfiable() && (c.test(accept) || c.intersects(&reach1)) {
            reach1.set(i);
        }
        // A globstar stays after taking a segment.
        if el.is_globstar() {
            c.set(i);
        }
        states[i].after = c;
    }
    let init = closure_of(&mut succ_of(edges, n, n), &states);
    Nfa {
        states: states.into_boxed_slice(),
        init,
        reach1,
    }
}

/// The sparse NFA over the graph: each position keeps its successors,
/// the start's last, the literal ones sorted by literal.
fn sparse_over(elems: &[Elem], ci: bool, edges: Option<&Edges>) -> SparseNfa {
    // The start is position `n`.
    let n = elems.len();

    // A position reaches accept through ≥ 1 segment iff its element
    // can consume one and what it enters reaches accept: from the last
    // position back, as successors always lie ahead.
    // (A nullable successor is a `**/`, which reaches accept through
    // one segment whenever it reaches it at all.)
    let mut reach1 = Wide::zero(Wide::words(2 * n + 2));
    for i in (0..n).rev() {
        if elems[i].satisfiable()
            && succ_of(edges, n, i).any(|t| t == ACCEPT || reach1.test(t as usize))
        {
            reach1.set(i);
        }
    }

    let mut at = Vec::with_capacity(3 * (n + 1) + 1);
    let mut succ = Vec::new();
    let (mut lits, mut nulls, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    let mut ends = Wide::zero(Wide::words(n + 1));
    for p in 0..=n {
        for t in succ_of(edges, n, p) {
            if t == ACCEPT {
                ends.set(p);
                continue;
            }
            if reach1.test(t as usize) {
                reach1.set(n + 1 + p);
            }
            let e = &elems[t as usize];
            if matches!(e, Elem::Lit(_)) {
                lits.push(t);
            } else if e.nullable() {
                nulls.push(t);
            } else {
                rest.push(t);
            }
        }
        // By literal, then position; a list holds no repeats, as the
        // walk deduplicates tails before it emits after them.
        lits.sort_unstable_by(|&a, &b| {
            let lit = |t: u32| elems[t as usize].as_lit();
            lit_cmp(lit(a), lit(b), ci).then(a.cmp(&b))
        });
        for part in [&mut lits, &mut nulls, &mut rest] {
            at.push(succ.len() as u32);
            succ.append(part);
        }
    }
    at.push(succ.len() as u32);

    SparseNfa {
        n: n as u32,
        at: at.into_boxed_slice(),
        succ: succ.into_boxed_slice(),
        ends,
        reach1,
    }
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

/// The NFA over the lone chain a matcher kept instead of one, and the
/// matcher it runs on: the chain's elements in path order (test hook).
pub(super) fn chain_nfa(m: &SegmentMatcher) -> (Cow<'_, SegmentMatcher>, AnyNfa) {
    let nodes = &m.chains[0].nodes;
    let m = if nodes.is_empty() {
        Cow::Borrowed(m)
    } else {
        let mut lone = m.clone();
        lone.elems = nodes.iter().map(|&i| m.elems[i as usize].clone()).collect();
        Cow::Owned(lone)
    };
    let nfa = any_nfa(&m.elems, m.case_insensitive, None);
    (m, nfa)
}

/// How many segment boundaries the pattern has (a floor for its
/// elements), whether it has a separator-crossing brace at all, and its
/// weight (`node_weight`).
fn survey(nodes: &[Node]) -> (usize, bool, usize) {
    let mut boundaries = 0;
    let mut linear = true;
    let mut weight = 0;
    for node in nodes {
        match node {
            Node::Brace(branches) => {
                // A brace crosses iff its branches hold a boundary.
                let before = boundaries;
                weight += 1;
                for b in branches {
                    let (n, _, w) = survey(b.seq());
                    boundaries += n;
                    weight += w;
                }
                linear &= boundaries == before;
            }
            Node::Literal(bytes) => weight += bytes.len(),
            Node::AnyChar | Node::Star | Node::Class(_) => weight += 1,
            Node::Separator | Node::Globstar => {
                boundaries += 1;
                weight += 1;
            }
            Node::Concat(_) => unreachable!("a sequence holds no sequence"),
        }
    }
    (boundaries, linear, weight)
}

/// What a node costs the walk to copy and compile: a literal its
/// length, a brace one plus its branches, anything else one.
pub(super) fn node_weight(node: &Node) -> usize {
    match node {
        Node::Literal(bytes) => bytes.len(),
        Node::Brace(branches) => {
            1 + branches
                .iter()
                .map(|b| b.seq().iter().map(node_weight).sum::<usize>())
                .sum::<usize>()
        }
        _ => 1,
    }
}

/// What the last node left behind for the next element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    /// Pattern start.
    Start,
    /// After a separator or a `**/`.
    Sep,
    /// A bare `**` absorbs the rest of the path.
    Open,
}

/// Paths under construction that agree on everything still to come:
/// the nodes awaiting a successor (`START` for the graph start), the
/// boundary just crossed and the in-segment nodes read since.
#[derive(Clone)]
struct Group<'a> {
    tails: Vec<u32>,
    state: Boundary,
    buf: Vec<&'a Node>,
    /// A `**/` read but not yet emitted: a bare `**` right after it
    /// absorbs it (`x/**/**` ≡ `x/**`), anything else makes it real.
    /// More of them add nothing (`**/**/` ≡ `**/`). While set, `state`
    /// is the boundary before it and `buf` holds at most the in-segment
    /// braces read since (`**/{a,b}x`), settled with the next node.
    pending_globstar: bool,
    /// A `**` ending a brace branch took the `/` after the brace as its
    /// `**/`: that separator closes nothing here.
    took_sep: bool,
    /// The `node_weight` of `buf`.
    weight: usize,
}

impl Group<'_> {
    fn first() -> Self {
        Self {
            tails: vec![START],
            state: Boundary::Start,
            buf: Vec::new(),
            pending_globstar: false,
            took_sep: false,
            weight: 0,
        }
    }

    /// What copying the group costs the walk.
    fn cost(&self) -> usize {
        1 + self.tails.len() + self.weight
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
        self.pending_globstar = false;
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
    /// The walk's work so far: nodes and their edges, the groups it
    /// copied and closed by their `cost` and weight, and the tables its
    /// wildcard segments built. Past `budget` the walk stops and the
    /// pattern is too complex.
    cost: usize,
    budget: usize,
    dot: bool,
    ci: bool,
}

impl Builder {
    fn new(boundaries: usize, linear: bool, dot: bool, ci: bool) -> Self {
        // A linear pattern stores no edges; a brace pattern has at least
        // one node per boundary, each with a predecessor and a key.
        let edges = if linear { 0 } else { boundaries + 1 };
        Builder {
            elems: Vec::with_capacity(boundaries + 1),
            linear,
            preds: Vec::with_capacity(edges),
            pred_at: Vec::with_capacity(edges),
            ends: Vec::new(),
            memo: HashMap::with_capacity_and_hasher(edges, Default::default()),
            cost: 0,
            budget: usize::MAX,
            dot,
            ci,
        }
    }

    fn finish(self, weight: usize) -> Graph {
        let edges = (!self.linear).then(|| self.successors());
        Graph {
            elems: self.elems,
            edges,
            weight,
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

    /// Invert the predecessor lists into CSR successor lists, the start's
    /// at `n` and the pattern's ends as edges to `ACCEPT`.
    fn successors(&self) -> Edges {
        let n = self.elems.len();
        let slot = |from: u32| if from == START { n } else { from as usize };
        let mut offsets = vec![0u32; n + 2];
        for &from in self.preds.iter().chain(&self.ends) {
            offsets[slot(from) + 1] += 1;
        }
        for i in 0..=n {
            offsets[i + 1] += offsets[i];
        }
        let mut fill = offsets.clone();
        let mut targets = vec![0u32; offsets[n + 1] as usize];
        for id in 0..=n {
            let (froms, to) = if id < n {
                (self.preds_of(id), id as u32)
            } else {
                (&self.ends[..], ACCEPT)
            };
            for &from in froms {
                let at = &mut fill[slot(from)];
                targets[*at as usize] = to;
                *at += 1;
            }
        }
        Edges { offsets, targets }
    }

    /// A node for `elem` after `tails`; an existing one when a branch
    /// already made it.
    fn emit(&mut self, elem: Elem, tails: &[u32]) -> u32 {
        self.cost += 1 + tails.len();
        let id = self.elems.len() as u32;
        if self.linear {
            self.elems.push(elem);
            return id;
        }
        let key = Self::key(&elem, tails);
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
    fn key(elem: &Elem, tails: &[u32]) -> Option<u64> {
        let mut h = Fnv::default();
        match elem {
            Elem::Lit(lit) => {
                h.write(&[0]);
                h.write(lit);
            }
            Elem::G0 => h.write(&[1]),
            Elem::G1 => h.write(&[2]),
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
            (Elem::G0, Elem::G0) | (Elem::G1, Elem::G1) => true,
            _ => false,
        }
    }

    /// Record that the pattern may end after `from`.
    fn end_after(&mut self, from: u32) {
        if !self.linear {
            self.ends.push(from);
        }
    }

    fn over(&self) -> bool {
        self.cost > self.budget
    }

    /// Walk a sequence of nodes; `sep_after` tells whether a `/` follows
    /// it outside (after the brace it is a branch of).
    fn walk<'a>(
        &mut self,
        nodes: &'a [Node],
        sep_after: bool,
        mut groups: Vec<Group<'a>>,
    ) -> Vec<Group<'a>> {
        let mut i = 0;
        while i < nodes.len() {
            if self.over() {
                break;
            }
            let node = &nodes[i];
            let sep_next = matches!(nodes.get(i + 1), Some(Node::Separator));
            // A `/` follows: next, or after the brace this is a branch of.
            let after = sep_next || (i + 1 == nodes.len() && sep_after);
            // Resolution puts a bare `**` only at the end of the pattern,
            // so no group is open here.
            debug_assert!(groups.iter().all(|g| g.state != Boundary::Open));
            // Nodes that may still be absorbed by a bare `**` keep the
            // pending `**/` elements; anything else makes them real (a
            // `/` through `close_all`, which skips the groups it is the
            // `**/` of).
            if !matches!(node, Node::Globstar | Node::Brace(_) | Node::Separator) {
                for g in groups.iter_mut() {
                    self.settle(g);
                }
            }
            match node {
                Node::Brace(branches) if crosses(node) => {
                    // A `**` ending a branch reads the `/` after the brace.
                    let mut out = Merged::default();
                    for branch in branches {
                        self.cost += groups.iter().map(Group::cost).sum::<usize>();
                        if self.over() {
                            break;
                        }
                        let copies = groups.to_vec();
                        for g in self.walk(branch.seq(), after, copies) {
                            out.add(g);
                        }
                    }
                    groups = out.groups;
                    for g in &mut groups {
                        g.dedupe_tails();
                    }
                }
                Node::Separator => self.close_all(&mut groups),
                // `**/`: zero or more whole segments, empty ones included,
                // so the separators around it are lenient (§9.5). It is
                // emitted later, when it is known that no bare `**`
                // absorbs it.
                Node::Globstar if after => {
                    for g in groups.iter_mut() {
                        // The parser never glues a `**` to text.
                        debug_assert!(g.buf.is_empty());
                        g.pending_globstar = true;
                        g.took_sep = !sep_next;
                    }
                    i += usize::from(sep_next);
                }
                // A bare `**` owns the rest of the path: pending `**/`
                // add nothing to it (the group continues without them),
                // and a separator before them still needs a segment to
                // follow (`x/**/**` ≡ `x/**`), so only the pattern start
                // gives `G0`.
                Node::Globstar => self.globstar_all(&mut groups),
                node => {
                    let weight = node_weight(node);
                    for g in groups.iter_mut() {
                        g.buf.push(node);
                        g.weight += weight;
                    }
                }
            }
            i += 1;
        }
        groups
    }

    /// Emit a group's pending `**/`. It is always a `G0`: it absorbs
    /// empty segments too, which makes the separators around it lenient.
    fn settle(&mut self, g: &mut Group<'_>) {
        if !g.pending_globstar {
            return;
        }
        let id = self.emit(Elem::G0, &g.tails);
        g.rebase(id, Boundary::Sep);
    }

    /// Close every group's segment into one node per group; they all
    /// continue as one group after the separator.
    fn close_all(&mut self, groups: &mut Vec<Group<'_>>) {
        self.join_all(groups, Boundary::Sep, |b, g| {
            b.settle(g);
            b.cost += g.weight;
            let elem = b.close(&g.buf);
            g.buf.clear();
            g.weight = 0;
            b.emit(elem, &g.tails)
        });
    }

    /// One globstar node per group, `G0` only at the pattern start;
    /// they continue as one open group.
    fn globstar_all(&mut self, groups: &mut Vec<Group<'_>>) {
        self.join_all(groups, Boundary::Open, |b, g| {
            // The parser never glues a `**` to text.
            debug_assert!(g.buf.is_empty());
            let elem = if g.state == Boundary::Start {
                Elem::G0
            } else {
                Elem::G1
            };
            b.emit(elem, &g.tails)
        });
    }

    /// Emit one node per group through `node`, then continue as one
    /// group at `state` (the first group's vectors carry on, in the
    /// same list). The branches a `**` ended pass through: they took
    /// this `/` already.
    fn join_all<'a>(
        &mut self,
        groups: &mut Vec<Group<'a>>,
        state: Boundary,
        node: impl Fn(&mut Self, &mut Group<'a>) -> u32,
    ) {
        let mut joined: Option<Group<'a>> = None;
        let mut taken = Vec::new();
        for mut g in groups.drain(..) {
            if g.took_sep {
                g.took_sep = false;
                taken.push(g);
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
        groups.extend(joined);
        groups.append(&mut taken);
        for g in groups.iter_mut() {
            g.dedupe_tails();
        }
    }

    /// The element of a closed segment. A wildcard's tables are charged
    /// to the walk: a brace pattern whose segments multiply fails early
    /// instead of keeping big tables in each of them.
    fn close(&mut self, buf: &[&Node]) -> Elem {
        // An escaped `\` on Windows: no segment holds a separator, so a
        // segment with one is a silent miss (§4.2), kept as a wildcard
        // that wants more bytes than any segment has.
        if buf
            .iter()
            .any(|node| matches!(node, Node::Literal(lit) if has_sep(lit)))
        {
            return Elem::Wild(Wild {
                kind: WildKind::Affix(Affix::new(Bytes::default(), Bytes::default())),
                min_len: NEVER,
                variable: false,
                dot_protect: false,
                any_seg: false,
            });
        }
        if !buf.iter().all(|node| matches!(node, Node::Literal(_))) {
            let wild = compile_wild(buf, self.dot, self.ci);
            self.cost += wild.table_bytes();
            return Elem::Wild(wild);
        }
        // Empty, one literal, or literals glued across a brace edge.
        Elem::Lit(concat_lits(buf))
    }
}

/// Groups joined when they agree on boundary, pending `**/` and partial
/// segment: their continuations are the same nodes. A union leaves a
/// group per member, so past `LINEAR_GROUPS` they are found by a hash
/// of that key.
#[derive(Default)]
struct Merged<'a> {
    groups: Vec<Group<'a>>,
    index: HashMap<u64, Vec<u32>, BuildHasherDefault<Fnv>>,
}

const LINEAR_GROUPS: usize = 16;

impl<'a> Merged<'a> {
    // Kept out of the recursive walk: inlined there, it costs the
    // unions of many branches 3–5% of their compile time.
    #[inline(never)]
    fn add(&mut self, g: Group<'a>) {
        let n = self.groups.len();
        if n == LINEAR_GROUPS {
            for (i, o) in self.groups.iter().enumerate() {
                self.index.entry(o.key()).or_default().push(i as u32);
            }
        }
        let found = if n < LINEAR_GROUPS {
            self.groups.iter().position(|o| o.agrees(&g))
        } else {
            let bucket = self.index.entry(g.key()).or_default();
            let found = bucket
                .iter()
                .map(|&i| i as usize)
                .find(|&i| self.groups[i].agrees(&g));
            if found.is_none() {
                bucket.push(n as u32);
            }
            found
        };
        match found {
            Some(i) => self.groups[i].tails.extend(g.tails),
            None => self.groups.push(g),
        }
    }
}

impl Group<'_> {
    fn agrees(&self, other: &Self) -> bool {
        self.state == other.state
            && self.pending_globstar == other.pending_globstar
            && self.took_sep == other.took_sep
            && self.buf.len() == other.buf.len()
            && self
                .buf
                .iter()
                .zip(&other.buf)
                .all(|(a, b)| std::ptr::eq(*a, *b) || a == b)
    }

    /// A hash of what `agrees` compares: each node by kind, a literal
    /// by its bytes too.
    fn key(&self) -> u64 {
        let mut h = Fnv::default();
        h.write(&[
            self.state as u8,
            self.pending_globstar as u8,
            self.took_sep as u8,
        ]);
        for node in &self.buf {
            std::mem::discriminant(*node).hash(&mut h);
            if let Node::Literal(bytes) = node {
                h.write(bytes);
            }
        }
        h.finish()
    }
}

/// The nodes with each crossing brace's in-segment branches as one
/// choice: `{a,b,/}` walks as `{{a,b},/}`. Forked one by one, such
/// branches give a group per partial segment, 2^k of them after k braces.
fn choices(nodes: &[Node]) -> Cow<'_, [Node]> {
    if !nodes.iter().any(splits_choices) {
        return Cow::Borrowed(nodes);
    }
    Cow::Owned(nodes.iter().map(regroup).collect())
}

/// `node` with its crossing braces' in-segment branches regrouped.
fn regroup(node: &Node) -> Node {
    match node {
        Node::Concat(children) => Node::Concat(choices(children).into_owned()),
        Node::Brace(branches) if crosses(node) => {
            let mut out: Vec<Node> = Vec::with_capacity(branches.len());
            let mut within: Vec<Node> = Vec::new();
            let mut at = None;
            for b in branches {
                if crosses(b) {
                    out.push(regroup(b));
                } else {
                    at.get_or_insert(out.len());
                    within.push(b.clone());
                }
            }
            if let Some(at) = at {
                let choice = match within.len() {
                    1 => within.pop().unwrap(),
                    _ => Node::Brace(within),
                };
                out.insert(at, choice);
            }
            Node::Brace(out)
        }
        other => other.clone(),
    }
}

/// Does a crossing brace in `node` have two or more in-segment branches?
fn splits_choices(node: &Node) -> bool {
    match node {
        Node::Brace(branches) => {
            crosses(node)
                && (branches.iter().filter(|b| !crosses(b)).count() > 1
                    || branches.iter().any(splits_choices))
        }
        Node::Concat(children) => children.iter().any(splits_choices),
        _ => false,
    }
}

/// Does the node hold a segment boundary?
fn crosses(node: &Node) -> bool {
    match node {
        Node::Separator | Node::Globstar => true,
        Node::Brace(xs) | Node::Concat(xs) => xs.iter().any(crosses),
        _ => false,
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

/// The bytes of a run of literal nodes, allocated once.
fn concat_lits(nodes: &[&Node]) -> Bytes {
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

fn compile_wild(nodes: &[&Node], dot: bool, ci: bool) -> Wild {
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
    // anything else runs the NFA.
    let Some(mut suffixes) = suffix_product(&nodes[idx..]) else {
        return SegNfa::wild(nodes, dot, ci);
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
        affix_set(prefix, suffixes, ci)
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
/// An `AffixSet` up to this size is scanned in pattern order; a
/// larger one is sorted and binary-searched per suffix length.
const LINEAR_SUFFIXES: usize = 8;
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
