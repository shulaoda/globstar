//! The NFA over the graph: one state per position, stepped once per
//! path segment. Dense rows in a `u64` up to 64 states, boxed word rows
//! while they fit `MAX_ROW_BYTES`, a sparse step beyond; it runs when
//! the chains cannot serve, and answers `match_dir` where it is kept.

use crate::bytes::{cmp_folded, seg_end};
use crate::dir_match::DirMatch;

use super::bits::{Bits, Wide};
use super::graph::{ACCEPT, Edges};
use super::{Elem, SegmentMatcher};

/// The wildcard-only segments `covers_below` feeds before it gives up:
/// a pattern needs about one more than its longest run of them, and
/// "not provable" is a valid answer (GLOB_SPEC §15.5).
const MAX_COVER_SEGMENTS: usize = 64;

/// An NFA over graph positions: one state per position plus the
/// accept state, which is the last.
#[derive(Debug, Clone)]
pub(super) struct Nfa<B: Bits> {
    /// Per state, the closure entered after consuming a segment there
    /// (a globstar's includes itself); the accept slot is never read.
    after: Box<[B]>,
    /// ε-closure of the start.
    init: B,
    /// States that can still consume ≥ 1 segment on a path to
    /// accept; drives `match_dir`'s prefix bit.
    reach1: B,
}

/// The same automaton past `MAX_ROW_BYTES` of rows, or with a literal
/// fan-out past `MAX_DENSE_LITS`, stepped sparsely. A position keeps its
/// successors, not a closure row, and the active set holds
/// states and whole positions: "every successor of `p` is entered" is
/// one entry, whose literals are found by one binary search. A step
/// costs what it tests, and the tables grow with the graph, not its
/// square.
#[derive(Debug, Clone)]
pub(super) struct SparseNfa {
    /// Positions, one per node (state `i` is position `i` is node `i`);
    /// the accept state is `n`.
    n: u32,
    /// Per position (the start last) its successors, in three parts:
    /// the literal ones sorted by literal (case-folded when
    /// insensitive), the nullable ones (`**/`), then the rest. Part `k`
    /// of position `p` is `succ[at[3p + k]..at[3p + k + 1]]`.
    at: Box<[u32]>,
    succ: Box<[u32]>,
    /// Positions the pattern may end after.
    ends: Wide,
    /// States, and positions with a successor (as `Active` indexes
    /// them, `n + 1 + p`), that can still consume ≥ 1 segment on a path
    /// to accept.
    reach1: Wide,
}

#[derive(Debug, Clone)]
pub(super) enum AnyNfa {
    Small(Nfa<u64>),
    // The rarer kinds are boxed, so the common one sets the size.
    Wide(Box<Nfa<Wide>>),
    Sparse(Box<SparseNfa>),
}

impl AnyNfa {
    pub(super) fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        match self {
            AnyNfa::Small(n) => n.is_match(m, path),
            AnyNfa::Wide(n) => n.is_match(m, path),
            AnyNfa::Sparse(n) => n.is_match(m, path),
        }
    }

    pub(super) fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        match self {
            AnyNfa::Small(n) => n.match_dir(m, dir_path),
            AnyNfa::Wide(n) => n.match_dir(m, dir_path),
            AnyNfa::Sparse(n) => n.match_dir(m, dir_path),
        }
    }
}

/// The NFA over the graph: dense rows in a `u64`, or past 64 states
/// within `MAX_ROW_BYTES` and `MAX_DENSE_LITS`; sparse beyond.
pub(super) fn any_nfa(elems: &[Elem], ci: bool, edges: Option<&Edges>) -> AnyNfa {
    let states = elems.len() + 1;
    if states <= 64 {
        AnyNfa::Small(dense_nfa(elems, edges))
    } else if states * Wide::words(states) * 8 <= MAX_ROW_BYTES
        && edges.is_none_or(|e| lit_fanout(elems, e) <= MAX_DENSE_LITS)
    {
        AnyNfa::Wide(Box::new(dense_nfa(elems, edges)))
    } else {
        AnyNfa::Sparse(Box::new(sparse_nfa(elems, ci, edges)))
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
/// of a graph with a crossing brace, else the next position.
fn succ_of(edges: Option<&Edges>, n: usize, p: usize) -> impl Iterator<Item = u32> + '_ {
    let (next, stored): (Option<u32>, &[u32]) = match edges {
        Some(e) => (None, e.succ(p)),
        None if p == n => (Some(0), &[]),
        None => (Some(if p + 1 < n { p as u32 + 1 } else { ACCEPT }), &[]),
    };
    next.into_iter().chain(stored.iter().copied())
}

/// The NFA over the graph (whose `edges` a crossing brace stores; a
/// linear pattern's positions follow one another). State `i` is
/// position `i`, the accept state last.
fn dense_nfa<B: Bits>(elems: &[Elem], edges: Option<&Edges>) -> Nfa<B> {
    let n = elems.len();
    let accept = n;
    let words = B::words(n + 1);
    // The rows are filled below.
    let mut after: Vec<B> = (0..=n).map(|_| B::zero(0)).collect();
    let mut reach1 = B::zero(words);

    // The closure some targets give, from the last position back:
    // successors always lie ahead, so each is complete when needed,
    // and a nullable successor contributes its own successors.
    let closure_of = |targets: &mut dyn Iterator<Item = u32>, after: &[B]| {
        let mut c = B::zero(words);
        for t in targets {
            if t == ACCEPT {
                c.set(accept);
                continue;
            }
            let t = t as usize;
            if elems[t].nullable() {
                c.or_with(&after[t]);
            }
            c.set(t);
        }
        c
    };
    for i in (0..n).rev() {
        let el = &elems[i];
        let mut c = closure_of(&mut succ_of(edges, n, i), &after);
        // A state reaches accept through ≥ 1 segment iff its element
        // can consume one and what follows reaches accept.
        if el.satisfiable() && (c.test(accept) || c.intersects(&reach1)) {
            reach1.set(i);
        }
        // A globstar stays after taking a segment.
        if el.is_globstar() {
            c.set(i);
        }
        after[i] = c;
    }
    let init = closure_of(&mut succ_of(edges, n, n), &after);
    Nfa {
        after: after.into_boxed_slice(),
        init,
        reach1,
    }
}

/// The sparse NFA over the graph: each position keeps its successors,
/// the start's last, the literal ones sorted by literal.
fn sparse_nfa(elems: &[Elem], ci: bool, edges: Option<&Edges>) -> SparseNfa {
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
            cmp_folded(lit(a), lit(b), ci).then(a.cmp(&b))
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

impl<B: Bits> Nfa<B> {
    #[inline]
    fn accept(&self) -> usize {
        self.after.len() - 1
    }

    pub(super) fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        self.run(m, path).test(self.accept())
    }

    pub(super) fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        // The empty dir is the walk root: nothing consumed yet.
        let root = dir_path.is_empty();
        let (mut active, exact) = if root {
            (self.init.clone(), m.is_match(dir_path))
        } else {
            let active = self.run(m, dir_path);
            let exact = active.test(self.accept());
            (active, exact)
        };
        let prefix = root || active.intersects(&self.reach1);
        // `dot=false` never covers a subtree: no wildcard matches the
        // dot-led names below.
        let all = m.dot && prefix && self.covers_below(m, &mut active);
        DirMatch::new(exact, prefix, all)
    }

    /// The active set after every segment of `path`.
    fn run(&self, m: &SegmentMatcher, path: &[u8]) -> B {
        let mut active = self.init.clone();
        let mut next = B::zero(B::words(self.after.len()));
        let mut pos = 0;
        while !active.is_zero() {
            let end = seg_end(path, pos);
            self.step(m, &active, &path[pos..end], &mut next);
            std::mem::swap(&mut active, &mut next);
            if end == path.len() {
                break;
            }
            pos = end + 1;
        }
        active
    }

    fn step(&self, m: &SegmentMatcher, active: &B, seg: &[u8], next: &mut B) {
        next.clear();
        let accept = self.accept();
        let absorb_ok = m.dot || seg.first() != Some(&b'.');
        active.for_each(|s| {
            if s == accept {
                return;
            }
            let consumes = match &m.elems[s] {
                Elem::Lit(lit) => m.lit_eq(lit, seg),
                Elem::Wild(w) => m.wild_consumes(w, seg),
                Elem::G0 | Elem::G1 => absorb_ok,
            };
            if consumes {
                next.or_with(&self.after[s]);
            }
        });
    }

    /// `step` over a non-empty segment that only wildcards consume:
    /// every literal and class rejects it (`dot=true` only).
    fn step_wild(&self, m: &SegmentMatcher, active: &B, next: &mut B) {
        next.clear();
        let accept = self.accept();
        active.for_each(|s| {
            if s != accept && m.elems[s].any_seg() {
                next.or_with(&self.after[s]);
            }
        });
    }

    /// Does every path below the directory match? `active` is the
    /// state set the directory's segments leave behind.
    ///
    /// Feeds wildcard-only segments until the set settles, requiring
    /// an accept after each one. A real segment fires every
    /// transition a wildcard-only one does, so `true` holds for all
    /// real paths; `false` may only mean "not provable".
    fn covers_below(&self, m: &SegmentMatcher, active: &mut B) -> bool {
        let mut next = B::zero(B::words(self.after.len()));
        // Tokens only move forward or sit on a globstar, so the set
        // settles within the number of states.
        for _ in 0..=self.after.len().min(MAX_COVER_SEGMENTS) {
            self.step_wild(m, active, &mut next);
            if !next.test(self.accept()) {
                return false;
            }
            if next == *active {
                return true;
            }
            std::mem::swap(active, &mut next);
        }
        false
    }
}

/// A sparse state set: states (`0..=n`, the accept state `n`) and whole
/// positions (every successor of position `p` entered, as `n + 1 + p`),
/// in one list to walk and one bit set to dedupe.
struct Active {
    items: Vec<u32>,
    has: Wide,
}

impl Active {
    fn new(nfa: &SparseNfa) -> Self {
        Active {
            items: Vec::new(),
            has: Wide::zero(Wide::words(2 * nfa.n as usize + 2)),
        }
    }

    fn has(&self, item: u32) -> bool {
        self.has.test(item as usize)
    }

    /// Empty it, touching only what it holds.
    fn clear(&mut self) {
        for &i in &self.items {
            self.has.unset(i as usize);
        }
        self.items.clear();
    }

    /// Add a state or a position; `false` when it was there already.
    fn add(&mut self, item: u32) -> bool {
        if self.has(item) {
            return false;
        }
        self.has.set(item as usize);
        self.items.push(item);
        true
    }

    fn same(&self, other: &Active) -> bool {
        self.items.len() == other.items.len() && self.items.iter().all(|&i| other.has(i))
    }
}

impl SparseNfa {
    #[inline]
    fn accept(&self) -> u32 {
        self.n
    }

    /// Position `p`'s successors from part `from` to part `to` (0 the
    /// literals, 1 the nullable ones, 2 the rest, 3 the end).
    fn succ(&self, p: usize, from: usize, to: usize) -> &[u32] {
        &self.succ[self.at[3 * p + from] as usize..self.at[3 * p + to] as usize]
    }

    pub(super) fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        self.run(m, path).has(self.accept())
    }

    pub(super) fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        // The empty dir is the walk root: nothing consumed yet.
        let root = dir_path.is_empty();
        let (mut active, exact) = if root {
            (self.init(), m.is_match(dir_path))
        } else {
            let active = self.run(m, dir_path);
            let exact = active.has(self.accept());
            (active, exact)
        };
        let prefix = root || active.items.iter().any(|&i| self.reach1.test(i as usize));
        // `dot=false` never covers a subtree: no wildcard matches the
        // dot-led names below.
        let all = m.dot && prefix && self.covers_below(m, &mut active);
        DirMatch::new(exact, prefix, all)
    }

    /// The start's closure.
    fn init(&self) -> Active {
        let mut active = Active::new(self);
        self.enter(self.n as usize, &mut active);
        active
    }

    /// The active set after every segment of `path`.
    fn run(&self, m: &SegmentMatcher, path: &[u8]) -> Active {
        let mut active = self.init();
        let mut next = Active::new(self);
        let mut pos = 0;
        while !active.items.is_empty() {
            let end = seg_end(path, pos);
            self.step(m, &active, &path[pos..end], &mut next);
            std::mem::swap(&mut active, &mut next);
            if end == path.len() {
                break;
            }
            pos = end + 1;
        }
        active
    }

    fn step(&self, m: &SegmentMatcher, active: &Active, seg: &[u8], next: &mut Active) {
        next.clear();
        let absorb_ok = m.dot || seg.first() != Some(&b'.');
        let consumes = |s: u32| match &m.elems[s as usize] {
            Elem::Lit(lit) => m.lit_eq(lit, seg),
            Elem::Wild(w) => m.wild_consumes(w, seg),
            Elem::G0 | Elem::G1 => absorb_ok,
        };
        for &item in &active.items {
            let Some(p) = item.checked_sub(self.n + 1) else {
                if item != self.accept() && consumes(item) {
                    self.fire(m, item, next);
                }
                continue;
            };
            let p = p as usize;
            // Equal literals sit together, one binary search away.
            let lits = self.succ(p, 0, 1);
            let order = |s: u32| cmp_folded(m.elems[s as usize].as_lit(), seg, m.case_insensitive);
            let first = lits.partition_point(|&s| order(s).is_lt());
            for &s in lits[first..].iter().take_while(|&&s| order(s).is_eq()) {
                self.fire(m, s, next);
            }
            for &s in self.succ(p, 1, 3) {
                if consumes(s) {
                    self.fire(m, s, next);
                }
            }
        }
    }

    /// State `s` took a segment: a globstar stays, and its position's
    /// successors are entered.
    fn fire(&self, m: &SegmentMatcher, s: u32, next: &mut Active) {
        if m.elems[s as usize].is_globstar() {
            next.add(s);
        }
        self.enter(s as usize, next);
    }

    /// Enter every successor of position `p`: the position itself
    /// stands for them, and through a nullable one (`**/`) its
    /// successors are entered too. The positions this adds are the
    /// work list: each is expanded once, as the walk reaches it.
    fn enter(&self, p: usize, next: &mut Active) {
        let base = self.n + 1;
        let mut i = next.items.len();
        if !next.add(base + p as u32) {
            return;
        }
        while let Some(&item) = next.items.get(i) {
            i += 1;
            let Some(p) = item.checked_sub(base) else {
                continue;
            };
            let p = p as usize;
            if self.ends.test(p) {
                next.add(self.accept());
            }
            for &t in self.succ(p, 1, 2) {
                next.add(base + t);
            }
        }
    }

    /// Does every path below the directory match? As `Nfa::covers_below`.
    fn covers_below(&self, m: &SegmentMatcher, active: &mut Active) -> bool {
        let mut next = Active::new(self);
        let accept = self.accept();
        let any_seg = |s: u32| m.elems[s as usize].any_seg();
        for _ in 0..=(self.n as usize + 1).min(MAX_COVER_SEGMENTS) {
            next.clear();
            for &item in &active.items {
                let Some(p) = item.checked_sub(self.n + 1) else {
                    if item != accept && any_seg(item) {
                        self.fire(m, item, &mut next);
                    }
                    continue;
                };
                // Literals never take a wildcard-only segment.
                for &s in self.succ(p as usize, 1, 3) {
                    if any_seg(s) {
                        self.fire(m, s, &mut next);
                    }
                }
            }
            if !next.has(accept) {
                return false;
            }
            if next.same(active) {
                return true;
            }
            std::mem::swap(active, &mut next);
        }
        false
    }
}
