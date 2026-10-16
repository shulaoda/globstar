//! In-segment mini NFA — the `WildKind::Generic` backend. A Thompson
//! NFA over the in-segment ops, simulated with a bit set: a `u64`
//! for up to 64 states, a word slice beyond, so no in-segment pattern
//! is too large.
//!
//! Every ε-edge points to a later state (see `compile_op`), so one
//! ascending sweep closes a set (`close`). A step ORs per-state closures
//! while they fit `MAX_CLOSURE_BYTES`; past that (a dense set per state
//! is quadratic in the states) it closes its set as it goes, so memory
//! stays linear in the pattern.

use crate::ast::{CharClass, ClassItem};
use crate::engine::ops::Op;

use super::bits::{Bits, Wide};

const UNSET: u32 = u32::MAX;

/// The most the per-state closures may take, about 2,900 states. Up to
/// there ORing dense closures is the faster step; past it, closing the
/// set as the step goes wins and keeps memory linear.
const MAX_CLOSURE_BYTES: usize = 1 << 20;

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
        let entry = b.compile_ops(ops.iter().copied()) as usize;
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
        entry: usize,
        dot: bool,
        kind: fn(SegNfaImpl<B>) -> Kind,
    ) -> Self {
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
        // closure is just `init` (DotGuards behave like Jumps).
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
        SegNfa {
            satisfiable,
            wild_led: !can_lit_dot,
            any_seg: nfa.any_seg(),
            kind: kind(nfa),
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
    /// Per-state ε-closure (guards pass — positions ≥ 1 are never
    /// segment starts); empty past `MAX_CLOSURE_BYTES`, where a step
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
    /// next — `?` and star bodies (segments contain no separators, so
    /// "any byte" ≡ "any non-separator byte" here).
    Any(u32),
    Split(u32, u32),
    Jump(u32),
    DotGuard(u32),
    Match,
}

/// A class as the bytes it takes, one bit each, with its negation applied
/// and the separators left out: a test is one lookup, however many items
/// the class lists. `negated` stays for dot protection, where a negated
/// class counts as a wildcard.
#[derive(Debug, Clone)]
struct ByteSet {
    bits: [u64; 4],
    negated: bool,
}

impl ByteSet {
    fn of(cls: &CharClass) -> Self {
        let mut bits = [0u64; 4];
        for &item in &cls.items {
            let (lo, hi) = match item {
                ClassItem::Byte(b) => (b, b),
                ClassItem::Range(lo, hi) => (lo, hi),
            };
            for b in lo..=hi {
                bits[usize::from(b >> 6)] |= 1 << (b & 63);
            }
        }
        if cls.negated {
            bits = bits.map(|w| !w);
        }
        for &b in b"/\\" {
            if std::path::is_separator(char::from(b)) {
                bits[usize::from(b >> 6)] &= !(1 << (b & 63));
            }
        }
        ByteSet {
            bits,
            negated: cls.negated,
        }
    }

    #[inline]
    fn has(&self, b: u8) -> bool {
        self.bits[usize::from(b >> 6)] >> (b & 63) & 1 != 0
    }
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
        if self.closures.is_empty() {
            for &c in seg {
                if active.is_zero() {
                    return false;
                }
                self.step_closing(&active, &mut next, Some(c));
                std::mem::swap(&mut active, &mut next);
            }
            return active.test(self.accept);
        }
        // The common case, written out: one OR of a kept closure per
        // consumer that fires (through `step_closing` it costs up to 50%).
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
                    if cls.has(c) {
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

    /// `next` = what `active` reaches over byte `c` (`None`: a byte only
    /// `Any` takes), without the kept closures: the consumers move, then
    /// `close` adds what their targets reach.
    fn step_closing(&self, active: &B, next: &mut B, c: Option<u8>) {
        next.clear();
        active.for_each(|s| match &self.states[s] {
            SegState::Byte(b, nx) if c == Some(*b) => next.set(*nx as usize),
            SegState::Class(cls, nx) if c.is_some_and(|c| cls.has(c)) => next.set(*nx as usize),
            SegState::Any(nx) => next.set(*nx as usize),
            _ => {}
        });
        close(&self.states, next, false);
    }

    /// Step the entry set through `Any` states only, one byte at a time,
    /// until it settles; every length from 1 up must accept.
    fn any_seg(&self) -> bool {
        let mut active = self.init.clone();
        let mut next = B::zero(self.words);
        // Every loop is a star's one-byte self-loop, so the set settles
        // within `states.len()` steps.
        for _ in 0..=self.states.len() {
            self.step_closing(&active, &mut next, None);
            if !next.test(self.accept) {
                return false;
            }
            if next == active {
                return true;
            }
            std::mem::swap(&mut active, &mut next);
        }
        false
    }
}

/// Can the entry reach `Match` through consumers some byte fires? Every
/// edge but a star's loop back to its own split points to a later
/// state, so one ascending sweep finds all the entry reaches.
fn satisfiable<B: Bits>(states: &[SegState], entry: usize, words: usize) -> bool {
    let mut reach = B::zero(words);
    reach.set(entry);
    for (s, state) in states.iter().enumerate().skip(entry) {
        if !reach.test(s) {
            continue;
        }
        match state {
            SegState::Match => return true,
            SegState::Split(a, b) => {
                reach.set(*a as usize);
                reach.set(*b as usize);
            }
            SegState::Class(cls, nx) => {
                if cls.bits != [0; 4] {
                    reach.set(*nx as usize);
                }
            }
            SegState::Byte(_, nx)
            | SegState::Any(nx)
            | SegState::Jump(nx)
            | SegState::DotGuard(nx) => reach.set(*nx as usize),
        }
    }
    false
}

/// Close `set` under ε-edges, keeping only consumers and `Match`. Every
/// ε-edge points to a later state, so one ascending sweep sees each
/// state after all that reach it; the word being swept stays in a
/// register, where most edges land. `block` builds the entry set of a
/// protected leading `.`: DotGuards are cut, and the consumers that
/// refuse that `.` (`Any`, negated classes) are dropped.
fn close<B: Bits>(states: &[SegState], set: &mut B, block: bool) {
    let words = set.words_mut();
    for w in 0..words.len() {
        let mut word = words[w];
        let mut todo = word;
        while todo != 0 {
            let bit = todo.isolate_lowest_one();
            todo ^= bit;
            let s = w * 64 + bit.trailing_zeros() as usize;
            let (a, b) = match &states[s] {
                SegState::Split(a, b) => (*a, *b),
                SegState::Jump(t) => (*t, UNSET),
                SegState::DotGuard(t) => (if block { UNSET } else { *t }, UNSET),
                SegState::Any(_) if block => (UNSET, UNSET),
                SegState::Class(cls, _) if block && cls.negated => (UNSET, UNSET),
                SegState::Byte(..) | SegState::Class(..) | SegState::Any(_) | SegState::Match => {
                    continue;
                }
            };
            word ^= bit;
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

/// Each state's ε-closure, in one backward pass: every ε-edge points to
/// a later state, whose closure is done.
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
                let s = self.alloc(SegState::Class(Box::new(ByteSet::of(cls)), UNSET));
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
            Some(cls) => self.alloc(SegState::Class(Box::new(ByteSet::of(&cls)), UNSET)),
            None => self.alloc(SegState::Byte(b, UNSET)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ByteSet;
    use crate::ast::Node;

    // The table takes exactly the bytes the class's items define, for
    // every byte, negated or not, with ranges and repeats.
    #[test]
    fn a_byte_set_takes_what_its_class_does() {
        for pattern in [
            "[a]",
            "[!a]",
            "[a-c]",
            "[!a-c]",
            "[]a]",
            "[]-]",
            "[a-]",
            "[\\]]",
            "[\\\\]",
            "[\x01-\x7f]",
            "[!\x01-\x7f]",
            "[bbbbbb]",
            "[a-cb-dz]",
            "[!\u{e9}]",
            "[\u{e9}-\u{ff}]",
        ] {
            let ast = crate::parser::parse(pattern.as_bytes()).unwrap();
            let Node::Class(cls) = &ast.body else {
                panic!("{pattern} is not one class: {:?}", ast.body);
            };
            let set = ByteSet::of(cls);
            for b in 0..=255u8 {
                assert_eq!(set.has(b), cls.matches(b), "{pattern} at byte {b:#04x}");
            }
        }
    }
}
