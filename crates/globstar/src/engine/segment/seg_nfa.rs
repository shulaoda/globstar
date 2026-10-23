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

use crate::parse::ast::{CharClass, Node};

use super::NEVER;
use super::bits::{Bits, Wide};
use super::wild::{Lens, Wild, WildKind};

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
        let mut nfa = SegNfaImpl {
            states,
            init,
            init_dot_blocked,
            closures,
            accept: n - 1,
            words,
            lens: Lens::default(),
        };
        nfa.lens = nfa.step_lens();
        Wild {
            min_len: if satisfiable { 0 } else { NEVER },
            variable: true,
            dot_protect: !dot && !can_lit_dot,
            // Does it accept every non-empty segment through its
            // wildcards alone? (`match_dir` all-below.)
            any_seg: nfa.lens.from == 1,
            kind: WildKind::Generic(Box::new(kind(nfa))),
        }
    }

    pub(super) fn matches(&self, seg: &[u8]) -> bool {
        match self {
            SegNfa::Small(n) => n.matches(seg),
            SegNfa::Wide(n) => n.matches(seg),
        }
    }

    /// What its wildcards alone take, by length (`Wild::lens`).
    pub(super) fn lens(&self) -> Lens {
        match self {
            SegNfa::Small(n) => n.lens,
            SegNfa::Wide(n) => n.lens,
        }
    }

    /// The states and the kept closures, in bytes (`Wild::table_bytes`).
    pub(super) fn table_bytes(&self) -> usize {
        match self {
            SegNfa::Small(n) => n.table_bytes(),
            SegNfa::Wide(n) => n.table_bytes(),
        }
    }
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
    /// Case-insensitively (`ci`) a listed ASCII letter lists its other
    /// case too, before the negation (§13).
    fn of(cls: &CharClass, ci: bool) -> Self {
        let mut set = ByteSet {
            bits: [0; 4],
            negated: cls.negated,
        };
        // A word at a time: a class costs its ranges, not the bytes they span.
        for &(lo, hi) in &cls.ranges {
            for w in lo >> 6..=hi >> 6 {
                let from = if w == lo >> 6 { lo & 63 } else { 0 };
                let to = if w == hi >> 6 { hi & 63 } else { 63 };
                set.bits[usize::from(w)] |= (u64::MAX >> (63 - (to - from))) << from;
            }
        }
        if ci {
            for b in b'A'..=b'Z' {
                if set.has(b) || set.has(b | 0x20) {
                    set.add(b);
                    set.add(b | 0x20);
                }
            }
        }
        if cls.negated {
            set.bits = set.bits.map(|w| !w);
        }
        for &b in b"/\\" {
            if crate::bytes::is_sep(b) {
                set.bits[usize::from(b >> 6)] &= !(1 << (b & 63));
            }
        }
        set
    }

    fn add(&mut self, b: u8) {
        self.bits[usize::from(b >> 6)] |= 1 << (b & 63);
    }

    #[inline]
    fn has(&self, b: u8) -> bool {
        self.bits[usize::from(b >> 6)] >> (b & 63) & 1 != 0
    }

    fn is_empty(&self) -> bool {
        self.bits == [0; 4]
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
    /// What its wildcards alone take, by length (see `step_lens`).
    lens: Lens,
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
}

impl<B: Bits> SegNfaImpl<B> {
    fn table_bytes(&self) -> usize {
        self.states.len() * size_of::<SegState>() + self.closures.len() * self.words * 8
    }

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
                self.step_closing(&active, &mut next, c);
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
                SegState::Any(nx, _) => next.or_with(&self.closures[*nx as usize]),
                _ => {}
            });
            std::mem::swap(&mut active, &mut next);
        }
        active.test(self.accept)
    }

    /// `next` = what `active` reaches over byte `c`, without the kept
    /// closures: the consumers move, then `close` adds what their
    /// targets reach.
    fn step_closing(&self, active: &B, next: &mut B, c: u8) {
        next.clear();
        active.for_each(|s| match &self.states[s] {
            SegState::Byte(b, nx) if *b == c => next.set(*nx as usize),
            SegState::Class(cls, nx) if cls.has(c) => next.set(*nx as usize),
            SegState::Any(nx, _) => next.set(*nx as usize),
            _ => {}
        });
        close(&self.states, next, false);
    }

    /// What the wildcards alone accept, by segment length (see `Lens`).
    fn step_lens(&self) -> Lens {
        // Most segments start with a literal or a class: no length.
        let mut any = false;
        self.init
            .for_each(|s| any |= matches!(self.states[s], SegState::Any(..)));
        if !any {
            return Lens::default();
        }
        wild_lengths(&self.states, &self.init)[self.accept].lens()
    }
}

/// The lengths of the segments the wildcards alone take to a state:
/// each length below 64 one bit, and `tail`, from which on every length
/// is (`u32::MAX`: none known). Lengths past 63 below the tail are left
/// out, which keeps out lengths only.
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

    /// The non-empty ones as a `Lens`: lengths 1 to 31, and where the
    /// run of every longer length starts, when that is by 32. The run
    /// is told only when the tail is by 64; one past that is left out.
    fn lens(self) -> Lens {
        let mut low = self.low;
        if self.tail < 64 {
            low |= u64::MAX << self.tail;
        }
        low &= !1;
        // Where the run through 63 starts (64: none does).
        let run = 64 - low.leading_ones();
        let from = if self.tail <= 64 && run <= 32 { run } else { 0 };
        let bits = low as u32;
        Lens {
            bits: if (1..32).contains(&from) {
                bits & ((1 << from) - 1)
            } else {
                bits
            },
            from,
        }
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
            SegState::Match => return true,
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
                SegState::Byte(..) | SegState::Class(..) | SegState::Any(..) | SegState::Match => {
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
            SegState::Split(..) | SegState::Match => unreachable!(),
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
    use super::{ByteSet, Lens, SegNfa};
    use crate::parse::ast::Node;

    // What the wildcards alone take, found in one pass over the states,
    // is what stepping them a byte at a time finds.
    #[test]
    fn wildcard_lengths_are_found_in_one_pass() {
        let lens = |pattern: &str| {
            let ast = crate::parse::parse(pattern.as_bytes()).unwrap();
            let nodes: Vec<&Node> = match &ast.body {
                Node::Concat(nodes) => nodes.iter().collect(),
                node => vec![node],
            };
            SegNfa::wild(&nodes, true, false).lens()
        };
        let of = |bits, from| Lens { bits, from };
        let run = |unit: &str, n: usize| unit.repeat(n);
        assert_eq!(lens(&run("{?,}", 100)), of(!1, 0));
        assert_eq!(lens(&(run("{,?}", 100) + "*")), Lens::ALL);
        assert_eq!(lens(&(run("?", 32) + "*")), of(0, 32));
        assert_eq!(lens(&(run("?", 33) + "*")), of(0, 0));
        assert_eq!(lens("{??,?}{??,?}"), of(0b11100, 0));
        assert_eq!(lens("{?,???*}"), of(0b10, 3));
        assert_eq!(lens("{?,???*}a"), of(0, 0));
        // A run that a star's lengths join below 64 goes on for good;
        // one they join past 63 is not told.
        let joined = |stars: usize| format!("{{{},{}*}}", run("{?,}", 80), run("?", stars));
        assert_eq!(lens(&joined(60)), Lens::ALL);
        assert_eq!(lens(&joined(70)), of(!1, 0));
    }

    // The table takes exactly the bytes the class's ranges define, for
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
            "[\x1f-\x21]",
            "[?-A]",
            "[bbbbbb]",
            "[a-cb-dz]",
            "[!\u{e9}]",
            "[\u{e9}-\u{ff}]",
        ] {
            let ast = crate::parse::parse(pattern.as_bytes()).unwrap();
            let Node::Class(cls) = &ast.body else {
                panic!("{pattern} is not one class: {:?}", ast.body);
            };
            let set = ByteSet::of(cls, false);
            for b in 0..=255u8 {
                assert_eq!(set.has(b), cls.matches(b), "{pattern} at byte {b:#04x}");
            }
        }
    }
}
