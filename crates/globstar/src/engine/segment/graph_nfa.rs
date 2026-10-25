//! The NFA over the graph: one state per position, stepped once per path
//! segment. Dense rows up to `MAX_ROW_BYTES`, a sparse step beyond.

use crate::bytes::{cmp_folded, seg_end};
use crate::dir_match::DirMatch;

use super::SegmentMatcher;
use super::bits::{Bits, Wide};
use super::graph::{ACCEPT, Edges, Elem, INNER};
use super::wild::GlueNfa;

/// Wildcard-only segments `covers_below` feeds before it gives up.
const MAX_COVER_SEGMENTS: usize = 64;

/// Dense rows pay while few states are active together: within this
/// many bytes of rows, and while no position leads to more than
/// `MAX_DENSE_LITS` literals, which the sparse step finds by one search.
const MAX_ROW_BYTES: usize = 4 * 1024;
const MAX_DENSE_LITS: usize = 48;

#[derive(Debug, Clone)]
pub(super) struct Nfa<B: Bits> {
    /// Per state, the closure entered after it takes a segment; a
    /// globstar's holds itself. The accept slot is never read.
    after: Box<[B]>,
    init: B,
    /// States from which at least one more segment reaches accept.
    reach1: B,
}

/// Each position keeps its successors instead of a closure row, and the
/// active set holds states and whole positions (`n + 1 + p`: every
/// successor of `p` is entered).
#[derive(Debug, Clone)]
pub(super) struct SparseNfa {
    /// States `0..n` are the positions; `n` is accept.
    n: u32,
    /// Position `p`'s successors in three parts, part `k` being
    /// `succ[at[3p + k]..at[3p + k + 1]]`: the literals sorted by
    /// literal, the nullable ones, the rest. The start is position `n`.
    at: Box<[u32]>,
    succ: Box<[u32]>,
    ends: Wide,
    /// States, and positions as `Active` indexes them, from which at
    /// least one more segment reaches accept.
    reach1: Wide,
    glue: Option<Box<GlueNfa>>,
}

#[derive(Debug, Clone)]
pub(super) enum AnyNfa {
    Small(Nfa<u64>),
    /// Boxed with `Sparse`, so the common kind sets the size.
    Wide(Box<Nfa<Wide>>),
    Sparse(Box<SparseNfa>),
}

/// States (`0..=n`, accept `n`) and positions (`n + 1 + p`), in one list
/// to walk and one bit set to dedupe.
#[derive(Clone)]
struct Active {
    items: Vec<u32>,
    has: Wide,
}

pub(super) fn any_nfa(
    elems: &[Elem],
    ci: bool,
    edges: Option<&Edges>,
    glue: Option<Box<GlueNfa>>,
) -> AnyNfa {
    let states = elems.len() + 1;
    if glue.is_some() {
        AnyNfa::Sparse(Box::new(sparse_nfa(elems, ci, edges, glue)))
    } else if states <= 64 {
        AnyNfa::Small(dense_nfa(elems, edges))
    } else if states * Wide::words(states) * 8 <= MAX_ROW_BYTES
        && edges.is_none_or(|e| lit_fanout(elems, e) <= MAX_DENSE_LITS)
    {
        AnyNfa::Wide(Box::new(dense_nfa(elems, edges)))
    } else {
        AnyNfa::Sparse(Box::new(sparse_nfa(elems, ci, edges, None)))
    }
}

fn lit_fanout(elems: &[Elem], edges: &Edges) -> usize {
    (0..=elems.len())
        .map(|i| {
            let succ = edges.succ(i).iter();
            succ.filter(|&&t| t != ACCEPT && matches!(elems[t as usize], Elem::Lit(_)))
                .count()
        })
        .fold(0, usize::max)
}

/// The successors of position `p` (`n`: the start) that start a segment:
/// the pieces glued to `p` are the glue NFA's.
fn succ_of<'a>(
    elems: &'a [Elem],
    edges: Option<&'a Edges>,
    p: usize,
) -> impl Iterator<Item = u32> + 'a {
    let n = elems.len();
    let (next, stored): (Option<u32>, &[u32]) = match edges {
        Some(e) => (None, e.succ(p)),
        None if p == n => (Some(0), &[]),
        None => (Some(if p + 1 < n { p as u32 + 1 } else { ACCEPT }), &[]),
    };
    let glued = |t: u32| t != ACCEPT && matches!(elems[t as usize], Elem::Piece(INNER));
    next.into_iter()
        .chain(stored.iter().copied().filter(move |&t| !glued(t)))
}

fn dense_nfa<B: Bits>(elems: &[Elem], edges: Option<&Edges>) -> Nfa<B> {
    let n = elems.len();
    let accept = n;
    let words = B::words(n + 1);
    let mut after: Vec<B> = (0..=n).map(|_| B::zero(0)).collect();
    let mut reach1 = B::zero(words);
    // Successors lie ahead, so a pass from the last position back finds
    // every closure complete; a join is only its successors.
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
            if !matches!(elems[t], Elem::Join) {
                c.set(t);
            }
        }
        c
    };
    for i in (0..n).rev() {
        let el = &elems[i];
        let mut c = closure_of(&mut succ_of(elems, edges, i), &after);
        if el.satisfiable() && (c.test(accept) || c.intersects(&reach1)) {
            reach1.set(i);
        }
        if el.is_globstar() {
            c.set(i);
        }
        after[i] = c;
    }
    let init = closure_of(&mut succ_of(elems, edges, n), &after);
    Nfa {
        after: after.into_boxed_slice(),
        init,
        reach1,
    }
}

fn sparse_nfa(
    elems: &[Elem],
    ci: bool,
    edges: Option<&Edges>,
    glue: Option<Box<GlueNfa>>,
) -> SparseNfa {
    let n = elems.len();
    // From the last position back: a position reaches accept through a
    // segment when its element takes one and a successor reaches it; a
    // piece when the glue NFA says so (`GlueNfa::reach`).
    let mut reach1 = Wide::zero(Wide::words(2 * n + 2));
    let mut glue_reach = None;
    for i in (0..n).rev() {
        let on = succ_of(elems, edges, i).any(|t| t == ACCEPT || reach1.test(t as usize));
        let reaches = match &elems[i] {
            Elem::Piece(entry) => {
                let glue = glue.as_deref().expect("pieces have a glue NFA");
                let good = glue_reach.get_or_insert_with(|| glue.set());
                glue.reach(i, on, good) && *entry != INNER
            }
            e => (e.satisfiable() || matches!(e, Elem::Join)) && on,
        };
        if reaches {
            reach1.set(i);
        }
    }

    let mut at = Vec::with_capacity(3 * (n + 1) + 1);
    let mut succ = Vec::new();
    let (mut lits, mut nulls, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    let mut ends = Wide::zero(Wide::words(n + 1));
    for p in 0..=n {
        for t in succ_of(elems, edges, p) {
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
        glue,
    }
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

impl<B: Bits> Nfa<B> {
    #[inline]
    fn accept(&self) -> usize {
        self.after.len() - 1
    }

    pub(super) fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        self.run(m, path).0.test(self.accept())
    }

    pub(super) fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        let root = dir_path.is_empty();
        let (mut active, mut next, exact) = if root {
            let next = B::zero(B::words(self.after.len()));
            (self.init.clone(), next, m.is_match(dir_path))
        } else {
            let (active, next) = self.run(m, dir_path);
            let exact = active.test(self.accept());
            (active, next, exact)
        };
        let prefix = root || active.intersects(&self.reach1);
        let all = m.dot && prefix && self.covers_below(m, &mut active, &mut next);
        DirMatch::new(exact, prefix, all)
    }

    /// The set after every segment of `path`, and the scratch set.
    fn run(&self, m: &SegmentMatcher, path: &[u8]) -> (B, B) {
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
        (active, next)
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
                Elem::Join => false,
                Elem::Piece(_) => unreachable!("pieces take the sparse NFA"),
            };
            if consumes {
                next.or_with(&self.after[s]);
            }
        });
    }

    /// `step` over a segment that only wildcards take (`dot=true` only).
    fn step_wild(&self, m: &SegmentMatcher, active: &B, next: &mut B) {
        next.clear();
        let accept = self.accept();
        active.for_each(|s| {
            if s != accept && m.elems[s].any_seg() {
                next.or_with(&self.after[s]);
            }
        });
    }

    /// Feeds wildcard-only segments until the set settles, requiring an
    /// accept after each: a real segment fires at least what one does, so
    /// `true` holds for every path below.
    fn covers_below(&self, m: &SegmentMatcher, active: &mut B, next: &mut B) -> bool {
        for _ in 0..=self.after.len().min(MAX_COVER_SEGMENTS) {
            self.step_wild(m, active, next);
            if !next.test(self.accept()) {
                return false;
            }
            if next == active {
                return true;
            }
            std::mem::swap(active, next);
        }
        false
    }
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

    fn clear(&mut self) {
        for &i in &self.items {
            self.has.unset(i as usize);
        }
        self.items.clear();
    }

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

    fn glue(&self) -> &GlueNfa {
        self.glue
            .as_deref()
            .expect("a piece comes with the glue NFA")
    }

    /// Position `p`'s successor parts `from..to` (0 literals, 1
    /// nullable, 2 the rest, 3 the end).
    fn succ(&self, p: usize, from: usize, to: usize) -> &[u32] {
        &self.succ[self.at[3 * p + from] as usize..self.at[3 * p + to] as usize]
    }

    fn note_piece(&self, m: &SegmentMatcher, s: u32, entries: &mut Option<Wide>) {
        if let Elem::Piece(entry) = m.elems[s as usize] {
            entries
                .get_or_insert_with(|| self.glue().set())
                .set(entry as usize);
        }
    }

    pub(super) fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        self.run(m, path).0.has(self.accept())
    }

    pub(super) fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        let root = dir_path.is_empty();
        let (mut active, mut next, exact) = if root {
            (self.init(), Active::new(self), m.is_match(dir_path))
        } else {
            let (active, next) = self.run(m, dir_path);
            let exact = active.has(self.accept());
            (active, next, exact)
        };
        let prefix = root || active.items.iter().any(|&i| self.reach1.test(i as usize));
        let all = m.dot && prefix && self.covers_below(m, &mut active, &mut next);
        DirMatch::new(exact, prefix, all)
    }

    fn init(&self) -> Active {
        let mut active = Active::new(self);
        self.enter(self.n as usize, &mut active);
        active
    }

    /// The set after every segment of `path`, and the scratch set.
    fn run(&self, m: &SegmentMatcher, path: &[u8]) -> (Active, Active) {
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
        (active, next)
    }

    fn step(&self, m: &SegmentMatcher, active: &Active, seg: &[u8], next: &mut Active) {
        next.clear();
        let absorb_ok = m.dot || seg.first() != Some(&b'.');
        // The pieces' entries are matched together, after the rest.
        let mut entries = None;
        let mut consumes = |s: u32| match &m.elems[s as usize] {
            Elem::Lit(lit) => m.lit_eq(lit, seg),
            Elem::Wild(w) => m.wild_consumes(w, seg),
            Elem::G0 | Elem::G1 => absorb_ok,
            Elem::Join => false,
            Elem::Piece(entry) => {
                entries
                    .get_or_insert_with(|| self.glue().set())
                    .set(*entry as usize);
                false
            }
        };
        for &item in &active.items {
            let Some(p) = item.checked_sub(self.n + 1) else {
                if item != self.accept() && consumes(item) {
                    self.fire(m, item, next);
                }
                continue;
            };
            let p = p as usize;
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
        if let Some(entries) = entries {
            self.glue()
                .exits(entries, seg, m.dot, |q| self.fire(m, q as u32, next));
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

    /// Enter position `p`, and through its nullable successors theirs;
    /// the positions added are the work list.
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

    /// As `Nfa::covers_below`; a piece fires the exits the glue NFA
    /// reaches through wildcards alone.
    fn covers_below(&self, m: &SegmentMatcher, active: &mut Active, next: &mut Active) -> bool {
        let accept = self.accept();
        let any_seg = |s: u32| m.elems[s as usize].any_seg();
        for _ in 0..=(self.n as usize + 1).min(MAX_COVER_SEGMENTS) {
            next.clear();
            let mut entries = None;
            for &item in &active.items {
                let Some(p) = item.checked_sub(self.n + 1) else {
                    if item != accept && any_seg(item) {
                        self.fire(m, item, next);
                    }
                    continue;
                };
                for &s in self.succ(p as usize, 1, 3) {
                    if any_seg(s) {
                        self.fire(m, s, next);
                    } else {
                        self.note_piece(m, s, &mut entries);
                    }
                }
            }
            if let Some(entries) = entries {
                self.glue()
                    .any_exits(&entries, |node| self.fire(m, node as u32, next));
            }
            if !next.has(accept) {
                return false;
            }
            if next.same(active) {
                return true;
            }
            std::mem::swap(active, next);
        }
        false
    }
}
