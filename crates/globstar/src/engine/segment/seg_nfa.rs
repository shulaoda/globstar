//! In-segment mini NFA — the `WildKind::Generic` backend. A Thompson
//! NFA over the in-segment nodes, simulated with a bit set: a `u64` for
//! up to 64 states, a word slice beyond, so no in-segment pattern is
//! too large. One state per node: a `*` takes any byte and stays, with
//! an ε-edge out (an `Any` that is its own `next`).
//!
//! Every ε-edge of the NFA points to a later state (see
//! `compile_node`), so one ascending sweep closes a set (`close`). A
//! step ORs per-state closures while they fit `MAX_CLOSURE_BYTES`; past
//! that (a dense set per state is quadratic in the states) it closes its
//! set as it goes, so memory stays linear in the pattern.
//!
//! The glued pieces' NFA (`GlueNfa`) is built of the same states, by
//! the same builder, and closed, stepped and measured by the same
//! passes.

use crate::parse::ast::Node;

use super::bits::{Bits, ByteSet, Wide};
use super::graph::{ACCEPT, Edges, Elem, INNER};
use super::wild::{NEVER, Wild, WildKind};

const UNSET: u32 = u32::MAX;

/// The most the per-state closures may take, about 1,000 states. Up to
/// there ORing dense closures is the faster step; past it, closing the
/// set as the step goes wins and keeps memory linear.
const MAX_CLOSURE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub(super) enum SegNfa {
    Small(SegNfaImpl<u64>),
    Wide(SegNfaImpl<Wide>),
}

impl SegNfa {
    /// The segment as a `Wild` over its NFA, with what the graph reads
    /// off it: one that accepts nothing gets `NEVER`, and it is
    /// dot-protected when nothing that may take a protected leading `.`
    /// (a literal `.`, a positive class listing it) can start a match.
    pub(super) fn wild(nodes: &[&Node], dot: bool, ci: bool) -> Wild {
        let mut b = SegBuilder {
            states: Vec::with_capacity(16),
            tails: Vec::new(),
            ci,
        };
        let entry = b.compile_nodes(nodes.iter().copied()) as usize;
        let accept = b.alloc(SegState::Match);
        for t in std::mem::take(&mut b.tails) {
            b.patch(t, accept);
        }
        let states = b.states.into_boxed_slice();
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
        // Under `dot` a leading `.` needs no protection, so the blocked
        // closure is just `init`.
        let init_dot_blocked = if dot { init.clone() } else { entry_set(true) };

        // Can any state of the protected entry set consume a leading
        // `.`? If not, a dot-led segment can never match under
        // dot=false and the whole matcher is protected.
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

#[derive(Debug, Clone)]
pub(super) struct SegNfaImpl<B: Bits> {
    states: Box<[SegState]>,
    /// ε-closure of the entry (used for all non-dot-led segments, and
    /// for the EOF/empty-segment accept).
    init: B,
    /// Entry ε-closure for a protected leading `.`: the consumers that
    /// refuse that `.` (`*`, `?`, negated classes) dropped, and no `*`
    /// passed.
    init_dot_blocked: B,
    /// Per-state ε-closure (positions ≥ 1 are never segment starts, so
    /// nothing is blocked); empty past `MAX_CLOSURE_BYTES`, where a step
    /// closes its set itself.
    closures: Box<[B]>,
    accept: usize,
    words: usize,
}

#[derive(Debug, Clone)]
enum SegState {
    /// byte, next
    Byte(u8, u32),
    /// class, next
    Class(Box<ByteSet>, u32),
    /// next, exit — takes any byte (segments contain no separators, so
    /// "any byte" ≡ "any non-separator byte" here). A `?` moves to
    /// `next` and has no `exit`; a `*` is its own `next`, so it stays,
    /// and leaves by the ε-edge `exit`. One kind for both keeps the hot
    /// loop at three arms.
    Any(u32, u32),
    Split(u32, u32),
    Jump(u32),
    Match,
    /// A glued piece's segment may end here, at graph node `.0` (see
    /// `GlueNfa`): kept in a set as `Match` is.
    Exit(u32),
}

impl<B: Bits> SegNfaImpl<B> {
    #[inline]
    fn matches(&self, seg: &[u8]) -> bool {
        // Under dot=true the blocked closure equals `init`, so the compile
        // already collapsed the dot decision into which set this picks.
        let protected_start = seg.first() == Some(&b'.');
        let active = if protected_start {
            self.init_dot_blocked.clone()
        } else {
            self.init.clone()
        };
        run(&self.states, &self.closures, active, seg, self.words).test(self.accept)
    }

    /// Do the wildcards alone take every non-empty segment (`match_dir`'s
    /// all-below)?
    fn any_seg(&self) -> bool {
        // Most segments start with a literal or a class: none.
        let mut any = false;
        self.init
            .for_each(|s| any |= matches!(self.states[s], SegState::Any(..)));
        any && wild_lengths(&self.states, &self.init)[self.accept].all_nonempty()
    }
}

/// The lengths of the segments the wildcards alone take to a state:
/// each length below 64 one bit, and `tail`, from which on every length
/// is (`u32::MAX`: none known). Lengths past 63 below the tail are left
/// out, which keeps out lengths only: a run of 70 `?` beside a `*` is
/// not told to take every length.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lengths {
    low: u64,
    tail: u32,
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

    /// As many more as a star takes: every length from the least on.
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

    /// Every non-empty length: 1 to 63 one by one, and every longer one
    /// through the tail.
    fn all_nonempty(self) -> bool {
        let run = if self.tail < 64 {
            u64::MAX << self.tail
        } else {
            0
        };
        self.tail <= 64 && (self.low | run | 1) == u64::MAX
    }
}

/// The lengths of the segments the wildcards alone take from the states
/// in `entry` to each state, in one pass in state order: every edge but
/// a star's loop onto itself points to a later state, so a state's are
/// all in when it is reached.
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
            // A `*` stays, then leaves.
            SegState::Any(nx, exit) if nx as usize == s => {
                here = here.star();
                at[s] = here;
                (exit, UNSET, here)
            }
            SegState::Any(nx, _) => (nx, UNSET, here.shift()),
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

/// Can the entry reach `Match` through consumers some byte fires? Every
/// edge but a star's loop onto itself points to a later state, so one
/// ascending sweep finds all the entry reaches.
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

/// Close `set` under ε-edges, keeping only consumers and `Match`. Every
/// ε-edge points to a later state, so one ascending sweep sees each
/// state after all that reach it; the word being swept stays in a
/// register, where most edges land. `block` builds the entry set of a
/// protected leading `.`: the consumers that refuse that `.` (`*`, `?`,
/// negated classes) are dropped, and no `*` is passed.
fn close<B: Bits>(states: &[SegState], set: &mut B, block: bool) {
    let words = set.words_mut();
    for w in 0..words.len() {
        let mut word = words[w];
        let mut todo = word;
        while todo != 0 {
            let bit = todo.isolate_lowest_one();
            todo ^= bit;
            let s = w * 64 + bit.trailing_zeros() as usize;
            // The ε-targets, and whether the state stays (a `*` is a
            // consumer with a way out).
            let (a, b, stays) = match &states[s] {
                SegState::Split(a, b) => (*a, *b, false),
                SegState::Jump(t) => (*t, UNSET, false),
                SegState::Any(..) if block => (UNSET, UNSET, false),
                // A `*` stays and leaves.
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

/// `active` after every byte of `seg`, or empty once nothing is left:
/// one OR of a kept closure per consumer that fires, or without kept
/// closures (`closures` empty) the consumers move and `close` adds what
/// their targets reach. Forced inline, one loop per way: it is the
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

/// Each state's ε-closure, in one backward pass: every ε-edge points to
/// a later state, whose closure is done (a `*` stays in its own).
fn closures<B: Bits>(states: &[SegState], words: usize) -> Box<[B]> {
    let n = states.len();
    // Filled back to front: state `t` sits at `n - 1 - t` until the
    // final reverse.
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

/// The pieces of every segment a separator-crossing brace cuts (see
/// `graph.rs`), as one NFA of the same states. A piece is its nodes'
/// states, then a fan of ε-edges: one to each piece glued after it,
/// and an `Exit` when its segment may end there. A join among pieces
/// is the fan alone. A segment is matched from the entry states of the
/// pieces the graph NFA holds, all at once, and the exits it reaches
/// are where the graph NFA goes on. Nodes are laid out in graph order,
/// and a piece is glued only to later ones, so every ε-edge still
/// points to a later state.
#[derive(Debug, Clone)]
pub(super) struct GlueNfa {
    states: Box<[SegState]>,
    /// Per-state ε-closures while they fit `MAX_CLOSURE_BYTES`, as in
    /// `SegNfaImpl`.
    closures: Box<[Wide]>,
    words: usize,
    /// Per graph node, the range of its states (empty outside the
    /// pieces).
    spans: Box<[(u32, u32)]>,
}

impl GlueNfa {
    /// The NFA of the nodes that are glued to their predecessors
    /// (`glued`) or have a successor glued to them, each of which becomes
    /// an `Elem::Piece`. `pieces` holds a wildcard node's nodes.
    pub(super) fn new(
        elems: &mut [Elem],
        edges: &Edges,
        glued: &[bool],
        pieces: &[Vec<&Node>],
        ci: bool,
    ) -> Self {
        let n = elems.len();
        let glued_to = |t: u32| t != ACCEPT && glued[t as usize];
        let mut b = SegBuilder {
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
            let first = b.states.len() as u32;
            let (lit, one);
            let nodes: &[&Node] = match &elems[i] {
                Elem::Lit(bytes) => {
                    lit = Node::Literal(bytes.to_vec());
                    one = [&lit];
                    &one
                }
                Elem::Wild(_) => &pieces[i],
                // A join is the fan alone.
                Elem::Join => &[],
                _ => unreachable!("a globstar is never glued"),
            };
            let entry = b.compile_nodes(nodes.iter().copied());
            for s in std::mem::take(&mut waiting[i]) {
                b.patch(s, entry);
            }
            let tails = std::mem::take(&mut b.tails);
            let exit = succ.iter().any(|&t| !glued_to(t));
            let glues = succ.iter().filter(|&&t| glued_to(t));
            let ways = usize::from(exit) + glues.clone().count();
            // Split `k` takes leaf `k` or the rest, as a brace's do.
            let fan = b.states.len();
            for _ in 1..ways {
                b.alloc(SegState::Split(UNSET, UNSET));
            }
            let mut leaves = Vec::with_capacity(ways);
            if exit {
                leaves.push(b.alloc(SegState::Exit(i as u32)));
            }
            for &t in glues {
                let s = b.alloc(SegState::Jump(UNSET));
                waiting[t as usize].push(s);
                leaves.push(s);
            }
            for k in 0..ways - 1 {
                let rest = if k + 2 < ways {
                    (fan + k + 1) as u32
                } else {
                    leaves[k + 1]
                };
                b.states[fan + k] = SegState::Split(leaves[k], rest);
            }
            let fan_entry = if ways > 1 { fan as u32 } else { leaves[0] };
            for t in tails {
                b.patch(t, fan_entry);
            }
            spans[i] = (first, b.states.len() as u32);
            elems[i] = Elem::Piece(if glued[i] { INNER } else { entry });
        }
        let states = b.states.into_boxed_slice();
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

    /// An empty set of its states.
    pub(super) fn set(&self) -> Wide {
        Wide::zero(self.words)
    }

    /// Calls `exit` with each node whose piece ends a match of `seg`
    /// begun at the entry states in `active`. A protected leading `.`
    /// (`dot` off) is taken only by name, as in `SegNfaImpl`.
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

    /// Calls `exit` with each node a piece of which the wildcards alone,
    /// from the entry states in `entries`, end every non-empty segment
    /// at (dot=true), as `SegNfaImpl::any_seg` reads a whole segment. A
    /// segment of a length reaches the nodes it does for every segment
    /// of it, so a node stands for all the segments between these
    /// entries and it, as a whole segment does.
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

    /// The states of `node` from which some segment reaches an exit
    /// that `good` holds for (`node`'s own when `exit_on`), added to
    /// `good`; whether its entry is one. Called for each node from the
    /// last back: every edge leads to a later state, so what a state
    /// leads to is known when it is reached.
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

struct SegBuilder {
    states: Vec<SegState>,
    tails: Vec<u32>,
    ci: bool,
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
            // A Split is never a dangling tail: a brace returns its
            // branches' leaf tails.
            SegState::Split(..) | SegState::Match | SegState::Exit(_) => unreachable!(),
        }
    }

    fn compile_nodes<'a>(&mut self, nodes: impl IntoIterator<Item = &'a Node>) -> u32 {
        let mut entry: Option<u32> = None;
        let mut pending: Vec<u32> = Vec::new();
        for node in nodes {
            let (op_entry, mut op_tails) = self.compile_node(node);
            for t in pending.drain(..) {
                self.patch(t, op_entry);
            }
            pending.append(&mut op_tails);
            if entry.is_none() {
                entry = Some(op_entry);
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
                // Its own `next`; the exit is patched in later.
                let s = self.states.len() as u32;
                self.alloc(SegState::Any(s, UNSET));
                (s, vec![s])
            }
            Node::Brace(branches) => {
                debug_assert!(!branches.is_empty());
                // The splits come before the branches, so every ε-edge
                // points to a later state (`close` and `closures` rely on it).
                let first = self.states.len();
                for _ in 1..branches.len() {
                    self.alloc(SegState::Split(UNSET, UNSET));
                }
                // `self.tails` is empty here (every caller drains it),
                // so the branch tails can be collected straight out of it.
                let mut entries = Vec::with_capacity(branches.len());
                let mut tails: Vec<u32> = Vec::new();
                for branch in branches {
                    entries.push(self.compile_nodes(branch.seq().iter()));
                    tails.append(&mut self.tails);
                }
                // Split `i` picks branch `i` or the rest: the next split,
                // or the last branch.
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
            // Separator-crossing nodes never appear inside a segment, and
            // a sequence is walked as its nodes.
            _ => unreachable!("separator-crossing node inside a segment"),
        }
    }

    fn lit_state(&mut self, b: u8) -> u32 {
        if !(self.ci && b.is_ascii_alphabetic()) {
            return self.alloc(SegState::Byte(b, UNSET));
        }
        // Case-insensitively a letter is the class of its two cases.
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
    use crate::parse::ast::Node;

    // Whether the wildcards alone take every non-empty segment is found
    // in one pass over the states, as stepping them a byte at a time
    // would find it.
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
        // A run that a star's lengths join below 64 goes on for good;
        // one they join past 63 is not told.
        let joined = |stars: usize| format!("{{{},{}*}}", run("{?,}", 80), run("?", stars));
        assert!(any_seg(&joined(60)));
        assert!(!any_seg(&joined(70)));
    }
}
