//! Ops → segment graph → chains and NFA.
//!
//! The walk keeps one *group* per distinct (boundary, partial segment)
//! reaching a point of the pattern, so a brace whose branches end at
//! a `/` costs one node per branch and the continuation is shared;
//! nothing is multiplied out. Branches that start alike share their
//! nodes too (a node is reused when its element and predecessors
//! already exist), which turns a union of names into a trie.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::engine::ops::Op;

use super::bits::{Bits, Wide};
use super::seg_nfa::SegNfa;
use super::{
    AffixIndex, AnyNfa, ByteSketch, Bytes, Chain, Elem, LINEAR_SUFFIXES, LitSet, LitSets, MANY_G,
    MAX_CHAINS, NO_G, Nfa, SegmentMatcher, State, SuffixRun, Wild, WildKind, facts,
};

/// Successor marker for "the pattern ends here".
const ACCEPT: u32 = u32::MAX;
/// Predecessor marker for the graph start.
const START: u32 = u32::MAX;

pub(super) fn compile(ops: &[Op], dot: bool, ci: bool) -> SegmentMatcher {
    let g = Graph::build(ops, dot, ci);
    let chains = g.chains();
    let n_states: usize = g.elems.iter().map(Elem::states).sum::<usize>() + 1;
    let nfa = if n_states <= 64 {
        AnyNfa::Small(g.nfa::<u64>())
    } else {
        AnyNfa::Wide(Box::new(g.nfa::<Wide>()))
    };
    let suffixes = facts::suffixes(ops);
    let suffix_last = ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci);
    SegmentMatcher {
        elems: g.elems.into_boxed_slice(),
        nfa,
        chains,
        suffixes,
        suffix_last,
        prefixes: facts::static_prefixes(ops),
        case_insensitive: ci,
        dot,
    }
}

/// The graph as the walk leaves it. A pattern without a
/// separator-crossing brace is one path, emitted in order: its edges
/// and start are implied and never stored.
struct Graph {
    elems: Vec<Elem>,
    linear: bool,
    /// Every node's predecessors, flat (`START` for the start); node
    /// `i` owns `preds[pred_at[i]..pred_at[i + 1]]`. Non-linear only.
    preds: Vec<u32>,
    pred_at: Vec<u32>,
    /// Nodes the pattern may end after.
    accept_preds: Vec<u32>,
    /// Successor lists in CSR form, derived from `preds`. Non-linear
    /// only.
    offsets: Vec<u32>,
    targets: Vec<u32>,
    start: Vec<u32>,
    ci: bool,
}

impl Graph {
    fn build(ops: &[Op], dot: bool, ci: bool) -> Self {
        let (boundaries, linear) = survey(ops);
        let mut b = Builder {
            g: Graph {
                elems: Vec::with_capacity(boundaries + 1),
                linear,
                preds: Vec::new(),
                pred_at: Vec::new(),
                accept_preds: Vec::new(),
                offsets: Vec::new(),
                targets: Vec::new(),
                start: Vec::new(),
                ci,
            },
            memo: HashMap::default(),
            dot,
        };
        let groups = b.walk(ops, vec![Group::first()]);
        for mut g in groups {
            b.settle(&mut g);
            if g.state == Boundary::Open {
                for &t in &g.tails {
                    b.link(t, ACCEPT);
                }
            } else {
                let elem = b.close(&g.buf);
                let id = b.emit(elem, &g.tails);
                b.link(id, ACCEPT);
            }
        }
        let mut g = b.g;
        if !linear {
            g.build_successors();
        }
        g
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
    fn build_successors(&mut self) {
        let n = self.elems.len();
        let mut offsets = vec![0u32; n + 1];
        for &from in self.preds.iter().chain(&self.accept_preds) {
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
        for &from in &self.accept_preds {
            if from != START {
                let slot = &mut fill[from as usize];
                targets[*slot as usize] = ACCEPT;
                *slot += 1;
            }
        }
        self.start = start;
        self.offsets = offsets;
        self.targets = targets;
    }

    fn succ(&self, i: usize) -> &[u32] {
        &self.targets[self.offsets[i] as usize..self.offsets[i + 1] as usize]
    }

    fn each_succ(&self, i: usize, f: &mut dyn FnMut(u32)) {
        if self.linear {
            f(if i + 1 < self.elems.len() {
                i as u32 + 1
            } else {
                ACCEPT
            });
        } else {
            for &t in self.succ(i) {
                f(t);
            }
        }
    }

    fn start(&self) -> &[u32] {
        if self.linear { &[0] } else { &self.start }
    }

    /// Every path through the graph as a `Chain`, in pattern order,
    /// or none when there are more than `MAX_CHAINS` or a chain with
    /// several globstars has more than 64 states (the NFA takes
    /// those).
    fn chains(&self) -> Box<[Chain]> {
        let n = self.elems.len();
        if self.linear {
            let chain = self.chain(Vec::new(), n, |i| i as u32, false);
            return chain.map_or(Box::from(&[][..]), |c| Box::new([c]));
        }
        let mut paths: Vec<Vec<u32>> = Vec::new();
        // Depth-first, first branch first.
        let mut stack: Vec<(u32, Vec<u32>)> =
            self.start.iter().rev().map(|&s| (s, Vec::new())).collect();
        while let Some((id, mut path)) = stack.pop() {
            if id == ACCEPT {
                paths.push(path);
                if paths.len() > MAX_CHAINS {
                    return Box::from(&[][..]);
                }
                continue;
            }
            path.push(id);
            let succ = self.succ(id as usize);
            for (k, &s) in succ.iter().enumerate().rev() {
                if k == 0 {
                    stack.push((s, path));
                    break;
                }
                stack.push((s, path.clone()));
            }
        }
        let several = paths.len() > 1;
        let mut chains = Vec::with_capacity(paths.len());
        for nodes in paths {
            let len = nodes.len();
            match self.chain(nodes.clone(), len, |i| nodes[i], several) {
                Some(c) => chains.push(c),
                None => return Box::from(&[][..]),
            }
        }
        chains.into_boxed_slice()
    }

    /// `stored` is what the chain keeps (empty for the linear
    /// identity); `at(i)` names the path's `i`th node.
    fn chain(
        &self,
        stored: Vec<u32>,
        len: usize,
        at: impl Fn(usize) -> u32,
        several: bool,
    ) -> Option<Chain> {
        let elem = |i: usize| &self.elems[at(i) as usize];
        let mut globstars = (0..len).filter(|&i| elem(i).is_globstar());
        let globstar = match (globstars.next(), globstars.next()) {
            (None, _) => NO_G,
            (Some(g), None) => g as u32,
            (Some(_), Some(_)) => MANY_G,
        };

        let nfa = if globstar == MANY_G && !self.linear {
            let states: usize = (0..len).map(|i| elem(i).states()).sum();
            if states + 1 > 64 {
                return None;
            }
            Some(Box::new(nfa_over::<u64>(
                &self.elems,
                self.ci,
                len,
                &at,
                |i, f| f(if i + 1 < len { i as u32 + 1 } else { ACCEPT }),
                &[0],
                false,
            )))
        } else {
            None
        };

        let mut joined_head = Bytes::empty();
        if globstar != NO_G && globstar != MANY_G && globstar > 0 {
            let head = 0..globstar as usize;
            if head.clone().all(|i| matches!(elem(i), Elem::Lit(_))) {
                let mut acc = Vec::new();
                for i in head {
                    if let Elem::Lit(lit) = elem(i) {
                        acc.extend_from_slice(lit);
                        acc.push(b'/');
                    }
                }
                joined_head = Bytes::new(&acc);
            }
        }

        let quick_suffix = match elem(len - 1) {
            Elem::Lit(lit) if several => lit.clone(),
            Elem::Wild(Wild {
                kind: WildKind::Affix { suffix, .. },
                ..
            }) if several => suffix.clone(),
            _ => Bytes::empty(),
        };

        Some(Chain {
            nodes: stored.into_boxed_slice(),
            globstar,
            nfa,
            joined_head,
            quick_suffix,
        })
    }

    /// The NFA over the whole graph: positions are node ids.
    fn nfa<B: Bits>(&self) -> Nfa<B> {
        nfa_over(
            &self.elems,
            self.ci,
            self.elems.len(),
            &|i| i as u32,
            |i, f| self.each_succ(i, f),
            self.start(),
            !self.linear,
        )
    }
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

/// Visits each successor of a position (`ACCEPT` included).
type Successors<'a> = &'a mut dyn FnMut(&mut dyn FnMut(u32));

/// Lay out the NFA over `n` positions, `node_at(i)` naming the
/// element at position `i`. States are numbered by position; the
/// accept state comes last. With `sets`, literal siblings (same
/// predecessor) are gathered into `LitSet`s.
fn nfa_over<B: Bits>(
    elems: &[Elem],
    ci: bool,
    n: usize,
    node_at: &dyn Fn(usize) -> u32,
    each_succ: impl Fn(usize, &mut dyn FnMut(u32)),
    start: &[u32],
    sets: bool,
) -> Nfa<B> {
    let elem = |i: usize| &elems[node_at(i) as usize];
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

    // The closure a position's successors give, from the last
    // position back: successors always lie ahead, so each is complete
    // when needed, and a nullable successor contributes its own
    // successors — its `after` minus its own states.
    let closure_of = |targets: Successors, states: &[State<B>]| {
        let mut c = B::zero(words);
        targets(&mut |t| {
            if t == ACCEPT {
                c.set(accept);
            } else {
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
        });
        c
    };
    for i in (0..n).rev() {
        let e = entries[i] as usize;
        let el = elem(i);
        states[e].node = node_at(i);
        let c = closure_of(&mut |f| each_succ(i, f), &states);
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
                let mut c = c;
                c.set(e);
                states[e].after = c;
            }
            Elem::G0Strict | Elem::G1 => {
                states[e + 1].node = node_at(i);
                let mut c = c;
                c.set(e + 1);
                states[e].after = c.clone();
                states[e + 1].after = c;
            }
        }
    }
    let init = closure_of(
        &mut |f| {
            for &s in start {
                f(s);
            }
        },
        &states,
    );

    // Literal siblings: the `Lit` successors of one position (or of
    // the start) when there are at least two.
    let mut lit_sets: Vec<LitSet<B>> = Vec::new();
    let mut lit_set_states = B::zero(words);
    if sets {
        let lit_of = |s: u32| match &elems[states[s as usize].node as usize] {
            Elem::Lit(lit) => &lit[..],
            _ => unreachable!(),
        };
        let mut lit_set = |targets: Successors| {
            let mut sorted: Vec<u32> = Vec::new();
            targets(&mut |t| {
                if t != ACCEPT && matches!(elem(t as usize), Elem::Lit(_)) {
                    sorted.push(entries[t as usize]);
                }
            });
            if sorted.len() < 2 {
                return;
            }
            sorted.sort_unstable_by(|&a, &b| lit_cmp(lit_of(a), lit_of(b), ci).then(a.cmp(&b)));
            sorted.dedup();
            let mut set_states = B::zero(words);
            for &s in &sorted {
                set_states.set(s as usize);
            }
            lit_set_states.or_with(&set_states);
            lit_sets.push(LitSet {
                states: set_states,
                sorted: sorted.into_boxed_slice(),
            });
        };
        lit_set(&mut |f| {
            for &s in start {
                f(s);
            }
        });
        for i in 0..n {
            lit_set(&mut |f| each_succ(i, f));
        }
    }

    Nfa {
        states: states.into_boxed_slice(),
        init,
        reach1,
        lit_sets: (!lit_sets.is_empty()).then(|| {
            Box::new(LitSets {
                sets: lit_sets.into_boxed_slice(),
                states: lit_set_states,
            })
        }),
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

/// Byte order, ASCII case folded when insensitive.
pub(super) fn lit_cmp(a: &[u8], b: &[u8], ci: bool) -> std::cmp::Ordering {
    if !ci {
        return a.cmp(b);
    }
    a.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(b.iter().map(u8::to_ascii_lowercase))
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
struct Group<'a> {
    tails: Vec<u32>,
    state: Boundary,
    buf: Vec<&'a Op>,
    /// `**/` elements read but not yet emitted: a bare `**` right after
    /// them absorbs them (`x/**/**` ≡ `x/**`), anything else makes
    /// them real. While non-zero, `state` is the boundary before the
    /// first of them and `buf` is empty.
    pending_oss: u32,
}

impl<'a> Group<'a> {
    fn first() -> Self {
        Self {
            tails: vec![START],
            state: Boundary::Start,
            buf: Vec::new(),
            pending_oss: 0,
        }
    }

    fn fork(&self) -> Self {
        Self {
            tails: self.tails.clone(),
            state: self.state,
            buf: self.buf.clone(),
            pending_oss: self.pending_oss,
        }
    }

    /// Continue as the one group after `id`, reusing the vectors. The
    /// partial segment stays: a pending `**/` settles ahead of it.
    fn rebase(&mut self, id: u32, state: Boundary) {
        self.tails.clear();
        self.tails.push(id);
        self.state = state;
        self.pending_oss = 0;
    }
}

struct Builder {
    g: Graph,
    /// Node reuse for branches: hash of (element, predecessors) →
    /// node. Non-linear only.
    memo: HashMap<u64, u32, BuildHasherDefault<Fnv>>,
    dot: bool,
}

impl Builder {
    /// A node for `elem` after `tails`; an existing one when a branch
    /// already made it.
    fn emit(&mut self, elem: Elem, tails: &[u32]) -> u32 {
        let id = self.g.elems.len() as u32;
        if self.g.linear {
            self.g.elems.push(elem);
            return id;
        }
        let key = self.key(&elem, tails);
        if let Some(&found) = key.and_then(|k| self.memo.get(&k)) {
            if self.same(found, &elem, tails) {
                return found;
            }
        }
        self.g.elems.push(elem);
        self.g.pred_at.push(self.g.preds.len() as u32);
        self.g.preds.extend_from_slice(tails);
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

    fn same(&self, id: u32, elem: &Elem, tails: &[u32]) -> bool {
        let i = id as usize;
        if self.g.preds_of(i) != tails {
            return false;
        }
        match (&self.g.elems[i], elem) {
            (Elem::Lit(a), Elem::Lit(b)) => a == b,
            (Elem::G0, Elem::G0) | (Elem::G0Strict, Elem::G0Strict) | (Elem::G1, Elem::G1) => true,
            _ => false,
        }
    }

    /// Record that the pattern may end after `from`.
    fn link(&mut self, from: u32, to: u32) {
        debug_assert_eq!(to, ACCEPT);
        if !self.g.linear {
            self.g.accept_preds.push(from);
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
                        let copies = groups.iter().map(Group::fork).collect();
                        for g in self.walk(branch, copies) {
                            merge(&mut out, g);
                        }
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
                        .filter(|g| g.state == Boundary::Strict && g.pending_oss == 0)
                    {
                        g.state = Boundary::Lenient;
                    }
                }
                Op::OptSegmentsSlash => {
                    for g in groups.iter_mut().filter(|g| g.state != Boundary::Open) {
                        self.flush_text(g);
                        g.pending_oss += 1;
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
                        g.pending_oss = 0;
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

    /// A `**` glued to text never survives the parser; close the text
    /// first should it ever happen.
    fn flush_text(&mut self, g: &mut Group<'_>) {
        debug_assert!(g.buf.is_empty());
        if !g.buf.is_empty() {
            let elem = self.close(&g.buf);
            g.buf.clear();
            let id = self.emit(elem, &g.tails);
            g.rebase(id, Boundary::Strict);
        }
    }

    /// Emit a group's pending `**/` elements: the first by the boundary
    /// before them, the rest strict (`**/**/x`).
    fn settle(&mut self, g: &mut Group<'_>) {
        if g.pending_oss == 0 || g.state == Boundary::Open {
            return;
        }
        let mut kind = match g.state {
            Boundary::Start | Boundary::Lenient => Elem::G0,
            _ => Elem::G0Strict,
        };
        for _ in 0..g.pending_oss {
            let id = self.emit(kind, &g.tails);
            g.rebase(id, Boundary::AfterG);
            kind = Elem::G0Strict;
        }
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
            b.flush_text(g);
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
    }

    fn close(&self, buf: &[&Op]) -> Elem {
        match buf {
            [] => Elem::Lit(Bytes::empty()),
            [Op::Lit(bytes)] => Elem::Lit(Bytes::new(bytes)),
            _ if buf.iter().all(|op| matches!(op, Op::Lit(_))) => {
                // Literal runs glued across a brace edge.
                let mut acc = Vec::new();
                for op in buf {
                    if let Op::Lit(bytes) = op {
                        acc.extend_from_slice(bytes);
                    }
                }
                Elem::Lit(Bytes::new(&acc))
            }
            _ => Elem::Wild(compile_wild(buf, self.dot, self.g.ci)),
        }
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

/// Does the op hold a segment boundary?
fn crosses(op: &Op) -> bool {
    match op {
        Op::Lit(_) | Op::AnyChar | Op::Star | Op::Class(_) => false,
        Op::Alternation(branches) => branches.iter().any(|b| b.iter().any(crosses)),
        _ => true,
    }
}

fn compile_wild(ops: &[&Op], dot: bool, ci: bool) -> Wild {
    // Leading literal run.
    let mut idx = 0;
    let prefix = match ops {
        [Op::Lit(bytes), ..] if !matches!(ops.get(1), Some(Op::Lit(_))) => {
            idx = 1;
            Bytes::new(bytes)
        }
        _ => {
            let mut acc = Vec::new();
            while let Some(Op::Lit(bytes)) = ops.get(idx) {
                acc.extend_from_slice(bytes);
                idx += 1;
            }
            Bytes::new(&acc)
        }
    };
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
            kind: WildKind::Affix {
                prefix,
                suffix: Bytes::empty(),
            },
            variable: has_star,
            dot_protect,
            any_seg: any_head,
        };
    }
    if let Some(suffixes) = suffix_product(&ops[idx..]) {
        let any_seg = any_head && suffixes.iter().any(|s| s.is_empty());
        if suffixes.len() == 1 {
            let suffix = Bytes::new(&suffixes[0]);
            return Wild {
                min_len: (prefix.len() + suffix.len()) as u32 + anychars,
                kind: WildKind::Affix { prefix, suffix },
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
/// factored union of names); a bigger product stays an NFA, which
/// holds the alternation without multiplying it out.
const MAX_SUFFIX_PRODUCT: usize = 1024;

fn suffix_product(ops: &[&Op]) -> Option<Vec<Vec<u8>>> {
    if let [Op::Lit(bytes)] = ops {
        return Some(vec![bytes.clone()]);
    }
    let mut parts: Vec<Vec<u8>> = vec![Vec::new()];
    for op in ops {
        match op {
            Op::Lit(bytes) => {
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
                if parts.len() * lits.len() > MAX_SUFFIX_PRODUCT {
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
fn affix_set(prefix: Bytes, mut suffixes: Vec<Vec<u8>>, ci: bool) -> WildKind {
    let last = ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci);
    if suffixes.len() <= LINEAR_SUFFIXES {
        return WildKind::AffixSet {
            prefix,
            suffixes: suffixes.iter().map(|s| Bytes::new(s)).collect(),
            last,
        };
    }
    if ci {
        for s in &mut suffixes {
            s.make_ascii_lowercase();
        }
    }
    suffixes.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    suffixes.dedup();
    let mut runs: Vec<SuffixRun> = Vec::new();
    for (i, s) in suffixes.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if run.len as usize == s.len() => run.end = i as u32 + 1,
            _ => runs.push(SuffixRun {
                len: s.len() as u32,
                start: i as u32,
                end: i as u32 + 1,
            }),
        }
    }
    WildKind::AffixIndex(Box::new(AffixIndex {
        prefix,
        suffixes: suffixes.iter().map(|s| Bytes::new(s)).collect(),
        last,
        runs: runs.into_boxed_slice(),
    }))
}
