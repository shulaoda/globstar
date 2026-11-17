//! The in-segment NFA (`WildKind::Generic`): a Thompson NFA over the
//! segment's nodes, simulated with a bit set. Every ε-edge points to a
//! later state, so one ascending sweep closes a set. The glue NFA of the
//! pieces a brace cuts is built of the same states (theory 03, §7.3 to
//! §7.5).

use std::ops::Range;

use crate::parse::ast::Node;

use super::bits::{Bits, ByteSet, Wide};
use super::graph::{ACCEPT, Edges, Elem, INNER};
use super::wild::{NEVER, Wild, WildKind};

const UNSET: u32 = u32::MAX;

/// The most the per-state closures may take (about 1,000 states); past
/// it a step closes its set as it goes, which keeps memory linear.
const MAX_CLOSURE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub(super) enum SegNfa {
    Small(SegNfaImpl<u64>),
    Wide(SegNfaImpl<Wide>),
}

#[derive(Debug, Clone)]
pub(super) struct SegNfaImpl<B: Bits> {
    states: Box<[SegState]>,
    init: B,
    /// The entry closure of a protected leading `.`: `*`, `?` and negated
    /// classes dropped, no `*` passed.
    init_dot_blocked: B,
    /// Per-state ε-closures; empty past `MAX_CLOSURE_BYTES`.
    closures: Box<[B]>,
    accept: usize,
    words: usize,
}

#[derive(Debug, Clone)]
enum SegState {
    Byte(u8, u32),
    Class(Box<ByteSet>, u32),
    /// Takes any byte. A `?` moves to `.0`; a `*` is its own `.0` and
    /// leaves by the ε-edge `.1`. One kind keeps the hot loop at three arms.
    Any(u32, u32),
    /// ε-edges to `.0` and `.1`; a jump leaves `.1` unset.
    Split(u32, u32),
    /// The segment may end here: the in-segment NFA's last state, or a
    /// glued piece's end at graph node `.0`.
    Exit(u32),
}

/// The lengths of the segments the wildcards alone take to a state: one
/// bit each below 64, and every length from `tail` on (`u32::MAX`: none).
/// Lengths past 63 below the tail are left out, which keeps out lengths
/// only.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lengths {
    low: u64,
    tail: u32,
}

/// The pieces of the segments a brace cuts, as one NFA: a piece's states,
/// then a fan of ε-edges to the pieces glued after it and an `Exit` where
/// its segment may end. A piece is glued only to later ones, so every
/// ε-edge still points to a later state.
#[derive(Debug, Clone)]
pub(super) struct GlueNfa {
    states: Box<[SegState]>,
    closures: Box<[Wide]>,
    words: usize,
    /// Per graph node, the range of its states.
    spans: Box<[(u32, u32)]>,
}

struct SegBuilder {
    states: Vec<SegState>,
    tails: Vec<u32>,
    ci: bool,
}

impl SegNfa {
    pub(super) fn wild(nodes: &[&Node], dot: bool, ci: bool) -> Wild {
        let mut builder = SegBuilder {
            states: Vec::with_capacity(16),
            tails: Vec::new(),
            ci,
        };
        let entry = builder.compile_nodes(nodes.iter().copied()) as usize;
        let accept = builder.alloc(SegState::Exit(UNSET));
        for t in std::mem::take(&mut builder.tails) {
            builder.patch(t, accept);
        }
        let states = builder.states.into_boxed_slice();
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
        let init_dot_blocked = if dot { init.clone() } else { entry_set(true) };
        // The segment is dot-protected when nothing can take a protected
        // leading `.` by name.
        let mut can_lit_dot = false;
        init_dot_blocked.for_each(|s| match &states[s] {
            SegState::Byte(x, _) => can_lit_dot |= *x == b'.',
            SegState::Class(cls, _) => can_lit_dot |= cls.has(b'.'),
            _ => {}
        });
        let mut good = B::zero(words);
        reach(&states, entry..n, true, &mut good);
        let satisfiable = good.test(entry);
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

    pub(super) fn states(&self) -> usize {
        match self {
            SegNfa::Small(n) => n.states.len(),
            SegNfa::Wide(n) => n.states.len(),
        }
    }
}

impl<B: Bits> SegNfaImpl<B> {
    #[inline]
    fn matches(&self, seg: &[u8]) -> bool {
        let active = if seg.first() == Some(&b'.') {
            self.init_dot_blocked.clone()
        } else {
            self.init.clone()
        };
        run(&self.states, &self.closures, active, seg, self.words).test(self.accept)
    }

    /// Do the wildcards alone take every non-empty segment?
    fn any_seg(&self) -> bool {
        let mut any = false;
        self.init
            .for_each(|s| any |= matches!(self.states[s], SegState::Any(..)));
        any && wild_lengths(&self.states, &self.init)[self.accept].all_nonempty()
    }
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

    /// Every length from the least on.
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

    fn all_nonempty(self) -> bool {
        let run = if self.tail < 64 {
            u64::MAX << self.tail
        } else {
            0
        };
        self.tail <= 64 && (self.low | run | 1) == u64::MAX
    }
}

/// The lengths from the states in `entry` to each state, in one pass in
/// state order: every edge but a star's loop points to a later state.
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
            SegState::Any(next, exit) if next as usize == s => {
                here = here.star();
                at[s] = here;
                (exit, UNSET, here)
            }
            SegState::Any(next, _) => (next, UNSET, here.shift()),
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

/// Adds to `good` the states of `span` from which some segment reaches
/// a state of `good`, or an exit when `exit_on`: from the last state
/// back, since every edge but a star's loop points to a later one.
fn reach<B: Bits>(states: &[SegState], span: Range<usize>, exit_on: bool, good: &mut B) {
    for s in span.rev() {
        let to = |t: u32| t != UNSET && t as usize != s && good.test(t as usize);
        let on = match &states[s] {
            SegState::Exit(_) => exit_on,
            SegState::Class(cls, _) if cls.is_empty() => false,
            SegState::Byte(_, n) | SegState::Class(_, n) => to(*n),
            SegState::Any(a, b) | SegState::Split(a, b) => to(*a) || to(*b),
        };
        if on {
            good.set(s);
        }
    }
}

/// Close `set` under ε-edges, keeping consumers and exits. The word
/// being swept stays in a register, where most edges land. `block`: the
/// entry set of a protected leading `.`.
fn close<B: Bits>(states: &[SegState], set: &mut B, block: bool) {
    let words = set.words_mut();
    for w in 0..words.len() {
        let mut word = words[w];
        let mut todo = word;
        while todo != 0 {
            let bit = todo.isolate_lowest_one();
            todo ^= bit;
            let s = w * 64 + bit.trailing_zeros() as usize;
            let (a, b, stays) = match &states[s] {
                SegState::Split(a, b) => (*a, *b, false),
                SegState::Any(..) if block => (UNSET, UNSET, false),
                SegState::Any(_, exit) if *exit != UNSET => (*exit, UNSET, true),
                SegState::Class(cls, _) if block && cls.negated => (UNSET, UNSET, false),
                SegState::Byte(..)
                | SegState::Class(..)
                | SegState::Any(..)
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

/// `active` after every byte of `seg`: through the kept closures, or
/// closing each step's set when there are none. Forced inline: it is the
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

/// Each state's ε-closure, in one backward pass.
fn closures<B: Bits>(states: &[SegState], words: usize) -> Box<[B]> {
    let n = states.len();
    // State `t` sits at `n - 1 - t` until the final reverse.
    let mut out: Vec<B> = Vec::with_capacity(n);
    for s in (0..n).rev() {
        let done = |t: u32| &out[n - 1 - t as usize];
        let c = match &states[s] {
            SegState::Split(a, b) => {
                let mut c = done(*a).clone();
                if *b != UNSET {
                    c.or_with(done(*b));
                }
                c
            }
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

impl GlueNfa {
    /// Of the nodes glued to their predecessors or with a successor glued
    /// to them, each of which becomes an `Elem::Piece`.
    pub(super) fn new(
        elems: &mut [Elem],
        edges: &Edges,
        glued: &[bool],
        pieces: &[Vec<&Node>],
        ci: bool,
    ) -> Self {
        let n = elems.len();
        let glued_to = |t: u32| t != ACCEPT && glued[t as usize];
        let mut builder = SegBuilder {
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
            let first = builder.states.len() as u32;
            let (lit, one);
            let nodes: &[&Node] = match &elems[i] {
                Elem::Lit(bytes) => {
                    lit = Node::Literal(bytes.to_vec());
                    one = [&lit];
                    &one
                }
                Elem::Wild(_) => &pieces[i],
                Elem::Join => &[],
                _ => unreachable!("a globstar is never glued"),
            };
            let entry = builder.compile_nodes(nodes.iter().copied());
            for s in std::mem::take(&mut waiting[i]) {
                builder.patch(s, entry);
            }
            let tails = std::mem::take(&mut builder.tails);
            let exit = succ.iter().any(|&t| !glued_to(t));
            let glues = succ.iter().filter(|&&t| glued_to(t));
            let ways = usize::from(exit) + glues.clone().count();
            let fan = builder.states.len();
            for _ in 1..ways {
                builder.alloc(SegState::Split(UNSET, UNSET));
            }
            let mut leaves = Vec::with_capacity(ways);
            if exit {
                leaves.push(builder.alloc(SegState::Exit(i as u32)));
            }
            for &t in glues {
                let s = builder.alloc(SegState::Split(UNSET, UNSET));
                waiting[t as usize].push(s);
                leaves.push(s);
            }
            let fan_entry = builder.fan(fan, &leaves);
            for t in tails {
                builder.patch(t, fan_entry);
            }
            spans[i] = (first, builder.states.len() as u32);
            elems[i] = Elem::Piece(if glued[i] { INNER } else { entry });
        }
        let states = builder.states.into_boxed_slice();
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

    pub(super) fn set(&self) -> Wide {
        Wide::zero(self.words)
    }

    /// Calls `exit` with each node whose piece ends a match of `seg` from
    /// the entry states in `active`.
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

    /// Calls `exit` with each node at which the wildcards alone, from the
    /// entry states in `entries`, end every non-empty segment.
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

    /// Adds to `good` the states of `node` from which some segment
    /// reaches an exit in `good` (`node`'s own when `exit_on`); whether
    /// its entry is one. Called from the last node back.
    pub(super) fn reach(&self, node: usize, exit_on: bool, good: &mut Wide) -> bool {
        let (lo, hi) = self.spans[node];
        reach(&self.states, lo as usize..hi as usize, exit_on, good);
        lo < hi && good.test(lo as usize)
    }
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
            | SegState::Split(n, _) => *n = target,
            SegState::Exit(_) => unreachable!(),
        }
    }

    /// The splits from `first` on fan out to `leaves`, split `k` to leaf
    /// `k` or the rest. Returns the fan's entry.
    fn fan(&mut self, first: usize, leaves: &[u32]) -> u32 {
        let last = leaves.len() - 1;
        for k in 0..last {
            let rest = if k + 1 < last {
                (first + k + 1) as u32
            } else {
                leaves[last]
            };
            self.states[first + k] = SegState::Split(leaves[k], rest);
        }
        if last == 0 { leaves[0] } else { first as u32 }
    }

    fn compile_nodes<'a>(&mut self, nodes: impl IntoIterator<Item = &'a Node>) -> u32 {
        let mut entry: Option<u32> = None;
        let mut pending: Vec<u32> = Vec::new();
        for node in nodes {
            let (node_entry, mut node_tails) = self.compile_node(node);
            for t in pending.drain(..) {
                self.patch(t, node_entry);
            }
            pending.append(&mut node_tails);
            if entry.is_none() {
                entry = Some(node_entry);
            }
        }
        self.tails.append(&mut pending);
        entry.unwrap_or_else(|| {
            // An empty branch: a jump that waits for what follows.
            let s = self.alloc(SegState::Split(UNSET, UNSET));
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
                let s = self.states.len() as u32;
                self.alloc(SegState::Any(s, UNSET));
                (s, vec![s])
            }
            Node::Brace(branches) => {
                debug_assert!(!branches.is_empty());
                // The splits come before the branches, so every ε-edge
                // points to a later state.
                let first = self.states.len();
                for _ in 1..branches.len() {
                    self.alloc(SegState::Split(UNSET, UNSET));
                }
                let mut entries = Vec::with_capacity(branches.len());
                let mut tails: Vec<u32> = Vec::new();
                for branch in branches {
                    entries.push(self.compile_nodes(branch.seq().iter()));
                    tails.append(&mut self.tails);
                }
                (self.fan(first, &entries), tails)
            }
            _ => unreachable!("separator-crossing node inside a segment"),
        }
    }

    fn lit_state(&mut self, b: u8) -> u32 {
        if !(self.ci && b.is_ascii_alphabetic()) {
            return self.alloc(SegState::Byte(b, UNSET));
        }
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
        // A run a star's lengths join below 64 goes on for good; one they
        // join past 63 is not told.
        let joined = |stars: usize| format!("{{{},{}*}}", run("{?,}", 80), run("?", stars));
        assert!(any_seg(&joined(60)));
        assert!(!any_seg(&joined(70)));
    }
}
