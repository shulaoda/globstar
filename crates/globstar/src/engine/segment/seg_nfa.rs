//! In-segment mini NFA — the `WildKind::Generic` backend. A Thompson
//! NFA over the in-segment ops, simulated with a bit set: a `u64`
//! for up to 64 states, a word slice beyond, so no in-segment pattern
//! is too large.

use crate::ast::CharClass;
use crate::engine::ops::Op;

use super::bits::{Bits, Wide};

const UNSET: u32 = u32::MAX;

#[derive(Debug, Clone)]
pub(super) struct SegNfa {
    kind: Kind,
    /// Does the NFA accept any segment at all? (`match_dir`
    /// satisfiability.)
    pub(super) satisfiable: bool,
    /// No entry-closure state can consume a leading `.` as a literal
    /// or positive class ⇒ the matcher is fully dot-protected.
    pub(super) wild_led: bool,
    /// Does the NFA accept every non-empty segment through its `Any`
    /// states alone? (`match_dir` all-below.)
    pub(super) any_seg: bool,
}

#[derive(Debug, Clone)]
enum Kind {
    Small(SegNfaImpl<u64>),
    Wide(SegNfaImpl<Wide>),
}

impl SegNfa {
    pub(super) fn compile(ops: &[&Op], dot: bool, ci: bool) -> Self {
        let mut b = SegBuilder {
            states: Vec::with_capacity(16),
            tails: Vec::new(),
            ci,
        };
        let entry = b.compile_ops(ops.iter().copied());
        let accept = b.alloc(SegState::Match);
        for t in std::mem::take(&mut b.tails) {
            b.patch(t, accept);
        }
        let states = b.states.into_boxed_slice();
        if states.len() <= 64 {
            Self::build(states, entry, dot, Kind::Small)
        } else {
            Self::build(states, entry, dot, Kind::Wide)
        }
    }

    fn build<B: Bits>(
        states: Box<[SegState]>,
        entry: u32,
        dot: bool,
        kind: fn(SegNfaImpl<B>) -> Kind,
    ) -> Self {
        let n = states.len();
        let words = B::words(n);
        let accept = n - 1;

        let closures = closures::<B>(&states, words);
        let init = closures[entry as usize].clone();
        // Under `dot` a leading `.` needs no protection, so the blocked
        // closure is just `init` (DotGuards behave like Jumps).
        let init_dot_blocked = if dot {
            init.clone()
        } else {
            blocked_closure(&states, entry as usize, words)
        };

        // Can any state of the protected entry set consume a leading
        // `.`? If not, a dot-led segment can never match under
        // dot=false and the whole matcher is protected.
        let mut can_lit_dot = false;
        init_dot_blocked.for_each(|s| match &states[s] {
            SegState::Byte(x, _) => can_lit_dot |= *x == b'.',
            SegState::Class(cls, _) => can_lit_dot |= cls.matches(b'.'),
            _ => {}
        });

        let satisfiable = satisfiable(&states, &closures, &init, accept, words);
        let any_seg = any_seg(&states, &closures, &init, accept, words);
        SegNfa {
            kind: kind(SegNfaImpl {
                states,
                init,
                init_dot_blocked,
                closures,
                accept,
                words,
            }),
            satisfiable,
            wild_led: !can_lit_dot,
            any_seg,
        }
    }

    pub(super) fn matches(&self, seg: &[u8]) -> bool {
        match &self.kind {
            Kind::Small(n) => n.matches(seg),
            Kind::Wide(n) => n.matches(seg),
        }
    }
}

#[derive(Debug, Clone)]
struct SegNfaImpl<B: Bits> {
    states: Box<[SegState]>,
    /// ε-closure of the entry with DotGuards passable (used for all
    /// non-dot-led segments, and for the EOF/empty-segment accept).
    init: B,
    /// Entry ε-closure for a protected leading `.`: DotGuards blocked
    /// and dot-protected consumers (`Any`, negated classes) dropped.
    init_dot_blocked: B,
    /// Per-state successor ε-closure (guards pass — positions ≥ 1 are
    /// never segment starts).
    closures: Box<[B]>,
    accept: usize,
    words: usize,
}

#[derive(Debug, Clone)]
enum SegState {
    /// byte, next
    Byte(u8, u32),
    /// class, next
    Class(Box<CharClass>, u32),
    /// next — `?` and star bodies (segments contain no separators, so
    /// "any byte" ≡ "any non-separator byte" here).
    Any(u32),
    Split(u32, u32),
    Jump(u32),
    DotGuard(u32),
    Match,
}

impl<B: Bits> SegNfaImpl<B> {
    #[inline]
    fn matches(&self, seg: &[u8]) -> bool {
        // Under dot=true the blocked closure equals `init`, so the compile
        // already collapsed the dot decision into which set this picks.
        let protected_start = seg.first() == Some(&b'.');
        let mut active = if protected_start {
            self.init_dot_blocked.clone()
        } else {
            self.init.clone()
        };
        let mut next = B::zero(self.words);
        for &c in seg {
            if active.is_zero() {
                return false;
            }
            next.clear();
            active.for_each(|s| match &self.states[s] {
                SegState::Byte(b, nx) => {
                    if *b == c {
                        next.or_with(&self.closures[*nx as usize]);
                    }
                }
                SegState::Class(cls, nx) => {
                    if cls.matches(c) {
                        next.or_with(&self.closures[*nx as usize]);
                    }
                }
                SegState::Any(nx) => next.or_with(&self.closures[*nx as usize]),
                _ => {}
            });
            std::mem::swap(&mut active, &mut next);
        }
        active.test(self.accept)
    }
}

/// Each state's ε-closure, guards passing. Every ε-edge points to a
/// later state (see `compile_op`), so one backward pass folds each
/// closure from its children's.
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
            SegState::Jump(t) | SegState::DotGuard(t) => done(*t).clone(),
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

/// The entry set for a segment that starts with a protected `.`:
/// DotGuards are cut, and the consumers that refuse that `.` (`Any`,
/// negated classes) are dropped. ε-edges point forward, so one sweep
/// from the entry finds every state it reaches.
fn blocked_closure<B: Bits>(states: &[SegState], entry: usize, words: usize) -> B {
    let mut reach = B::zero(words);
    let mut out = B::zero(words);
    reach.set(entry);
    for (s, state) in states.iter().enumerate().skip(entry) {
        if !reach.test(s) {
            continue;
        }
        match state {
            SegState::Split(a, b) => {
                reach.set(*a as usize);
                reach.set(*b as usize);
            }
            SegState::Jump(t) => reach.set(*t as usize),
            SegState::DotGuard(_) | SegState::Any(_) => {}
            SegState::Class(cls, _) if cls.negated => {}
            SegState::Byte(..) | SegState::Class(..) | SegState::Match => out.set(s),
        }
    }
    out
}

/// Can the entry set reach accept? States some byte fires are
/// followed through their successor closures until nothing new
/// appears.
fn satisfiable<B: Bits>(
    states: &[SegState],
    closures: &[B],
    init: &B,
    accept: usize,
    words: usize,
) -> bool {
    let mut fires = B::zero(words);
    for (s, st) in states.iter().enumerate() {
        let some_byte = match st {
            SegState::Byte(..) | SegState::Any(_) => true,
            SegState::Class(cls, _) => (0u16..=255).any(|b| cls.matches(b as u8)),
            _ => false,
        };
        if some_byte {
            fires.set(s);
        }
    }
    let next_of = |s: usize| match &states[s] {
        SegState::Byte(_, n) | SegState::Class(_, n) | SegState::Any(n) => *n as usize,
        _ => unreachable!(),
    };
    let mut reach = init.clone();
    let mut work: Vec<usize> = Vec::new();
    init.for_each(|s| {
        if fires.test(s) {
            work.push(s);
        }
    });
    while let Some(s) = work.pop() {
        closures[next_of(s)].for_each(|t| {
            if !reach.test(t) {
                reach.set(t);
                if fires.test(t) {
                    work.push(t);
                }
            }
        });
    }
    reach.test(accept)
}

/// Step the entry set through `Any` states only, one byte at a time,
/// until it settles; every length from 1 up must accept.
fn any_seg<B: Bits>(
    states: &[SegState],
    closures: &[B],
    init: &B,
    accept: usize,
    words: usize,
) -> bool {
    let mut active = init.clone();
    let mut next = B::zero(words);
    // Every loop is a star's one-byte self-loop, so the set settles
    // within `states.len()` steps.
    for _ in 0..=states.len() {
        next.clear();
        active.for_each(|s| {
            if let SegState::Any(nx) = &states[s] {
                next.or_with(&closures[*nx as usize]);
            }
        });
        if !next.test(accept) {
            return false;
        }
        if next == active {
            return true;
        }
        std::mem::swap(&mut active, &mut next);
    }
    false
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

    fn patch(&mut self, state: u32, target: u32) {
        match &mut self.states[state as usize] {
            SegState::Byte(_, n)
            | SegState::Class(_, n)
            | SegState::Any(n)
            | SegState::Jump(n)
            | SegState::DotGuard(n) => *n = target,
            // A Split is never a dangling tail: Star returns its exit state
            // and Alternation returns its branches' leaf tails.
            SegState::Split(..) | SegState::Match => unreachable!(),
        }
    }

    fn compile_ops<'a>(&mut self, ops: impl IntoIterator<Item = &'a Op>) -> u32 {
        let mut entry: Option<u32> = None;
        let mut pending: Vec<u32> = Vec::new();
        for op in ops {
            let (op_entry, mut op_tails) = self.compile_op(op);
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

    fn compile_op(&mut self, op: &Op) -> (u32, Vec<u32>) {
        match op {
            Op::Lit(bytes) => {
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
            Op::AnyChar => {
                let s = self.alloc(SegState::Any(UNSET));
                (s, vec![s])
            }
            Op::Class(cls) => {
                let s = self.alloc(SegState::Class(Box::new(cls.clone()), UNSET));
                (s, vec![s])
            }
            Op::Star => {
                let entry = self.alloc(SegState::Split(UNSET, UNSET));
                let body = self.alloc(SegState::Any(entry));
                // Under `dot` the guard is inert (blocked closures are
                // never built), so DotGuard serves both compiles.
                let exit = self.alloc(SegState::DotGuard(UNSET));
                if let SegState::Split(a, b) = &mut self.states[entry as usize] {
                    *a = body;
                    *b = exit;
                }
                (entry, vec![exit])
            }
            Op::Alternation(branches) => {
                debug_assert!(!branches.is_empty());
                // The splits come before the branches, so every ε-edge
                // points to a later state (`closures` relies on it).
                let first = self.states.len();
                for _ in 1..branches.len() {
                    self.alloc(SegState::Split(UNSET, UNSET));
                }
                // `self.tails` is empty here (every caller drains it),
                // so the branch tails can be collected straight out of it.
                let mut entries = Vec::with_capacity(branches.len());
                let mut tails: Vec<u32> = Vec::new();
                for branch in branches {
                    entries.push(self.compile_ops(branch.iter()));
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
            // Separator-crossing ops never appear inside a segment.
            _ => unreachable!("separator-crossing op inside a segment"),
        }
    }

    fn lit_state(&mut self, b: u8) -> u32 {
        let class = self.ci.then(|| CharClass::ci_letter(b)).flatten();
        match class {
            Some(cls) => self.alloc(SegState::Class(Box::new(cls), UNSET)),
            None => self.alloc(SegState::Byte(b, UNSET)),
        }
    }
}
