//! In-segment mini NFA — the [`super::WildKind::Generic`] matcher.
//!
//! Thompson-lite over in-segment ops. ≤ 64 states, `u64` active set,
//! per-state successor ε-closures precomputed. Dot protection is
//! realized exactly as in the byte engines: offset 0 is the only
//! segment start inside a segment, so DotGuards block there when the
//! first byte is `.`, and dot-protected consumers refuse that `.`.

use crate::ast::{CharClass, ClassItem};
use crate::engine::eq_byte;
use crate::engine::ops::Op;
use crate::options::ascii_case_alt;

/// In-segment NFA state budget (active set is one `u64`).
const MAX_SEG_NFA_STATES: usize = 64;

const UNSET: u8 = u8::MAX;

#[derive(Debug, Clone)]
pub(super) struct SegNfa {
    states: Box<[SegState]>,
    /// ε-closure of the entry with DotGuards passable (used for all
    /// non-dot-led segments, and for the EOF/empty-segment accept).
    init: u64,
    /// ε-closure of the entry with DotGuards blocked (dot-led segment
    /// under a dot=false compile).
    init_dot_blocked: u64,
    /// Per-state successor ε-closure (guards pass — positions ≥ 1 are
    /// never segment starts).
    closures: Box<[u64]>,
    accept_mask: u64,
    /// Does the NFA accept any segment at all? (`match_dir`
    /// satisfiability.)
    pub(super) satisfiable: bool,
    /// No entry-closure state can consume a leading `.` as a literal
    /// or positive class ⇒ the matcher is fully dot-protected.
    pub(super) wild_led: bool,
    /// Compile-time dot option (drives the offset-0 gates).
    dot: bool,
}

#[derive(Debug, Clone)]
enum SegState {
    /// byte, next
    Byte(u8, u8),
    /// class, next, dot_protected (negated class under dot=false)
    Class(Box<CharClass>, u8, bool),
    /// next, dot_protected — `?` and star bodies (segments contain no
    /// separators, so "any byte" ≡ "any non-separator byte" here).
    Any(u8, bool),
    Split(u8, u8),
    Jump(u8),
    DotGuard(u8),
    Match,
}

impl SegNfa {
    /// `None` when the ops exceed the state budget or contain a
    /// separator-crossing op (which never appears in-segment).
    pub(super) fn compile(ops: &[Op], dot: bool, ci: bool) -> Option<Box<Self>> {
        let mut b = SegBuilder {
            states: Vec::with_capacity(16),
            tails: Vec::new(),
            ci,
        };
        let entry = b.compile_ops(ops, dot)?;
        let accept = b.alloc(SegState::Match)?;
        for t in std::mem::take(&mut b.tails) {
            b.patch(t, accept);
        }
        let states = b.states.into_boxed_slice();
        let n = states.len();

        // Memoized guard-passing closures: the ε-graph (Split/Jump/
        // Guard edges) is acyclic — every pattern loop passes through
        // a consumer — so each state's closure folds from its
        // children exactly once.
        let mut closures = vec![u64::MAX; n];
        for s in 0..n {
            memo_closure(&states, &mut closures, s);
        }
        let init = closures[entry as usize];
        let init_dot_blocked = closure_of_dot_blocked(&states, entry as usize);
        let accept_mask = 1u64 << (n - 1);

        // wild_led: can any entry state consume `.` as a literal or
        // positive class? If not, a dot-led segment can never match
        // under dot=false and the whole matcher is protected.
        let mut can_lit_dot = false;
        let mut bits = init;
        while bits != 0 {
            let s = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            match &states[s] {
                SegState::Byte(x, _) => can_lit_dot |= *x == b'.',
                SegState::Class(cls, _, dp) => can_lit_dot |= !*dp && cls.matches(b'.'),
                _ => {}
            }
        }
        let wild_led = !can_lit_dot;

        let satisfiable = compute_satisfiable(&states, &closures, init, accept_mask);

        Some(Box::new(Self {
            states,
            init,
            init_dot_blocked,
            closures: closures.into_boxed_slice(),
            accept_mask,
            satisfiable,
            wild_led,
            dot,
        }))
    }

    /// Match a whole segment.
    pub(super) fn matches(&self, seg: &[u8]) -> bool {
        let protected_start = !self.dot && !seg.is_empty() && seg[0] == b'.';
        let mut active = if protected_start {
            self.init_dot_blocked
        } else {
            self.init
        };
        for (idx, &c) in seg.iter().enumerate() {
            if active == 0 {
                return false;
            }
            let guard_dot = idx == 0 && !self.dot && c == b'.';
            let mut next: u64 = 0;
            let mut bits = active;
            while bits != 0 {
                let s = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                match &self.states[s] {
                    SegState::Byte(b, nx) => {
                        if eq_byte::<false>(*b, c) {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    SegState::Class(cls, nx, dp) => {
                        if cls.matches(c) && !(*dp && guard_dot) {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    SegState::Any(nx, dp) => {
                        if !(*dp && guard_dot) {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    _ => {}
                }
            }
            active = next;
        }
        active & self.accept_mask != 0
    }
}

/// Memoized guard-passing closure. `u64::MAX` marks "uncomputed" — a
/// real closure can never be all-ones (Split states are never closure
/// members, and a splitless NFA has single-bit closures).
fn memo_closure(states: &[SegState], memo: &mut [u64], s: usize) -> u64 {
    if memo[s] != u64::MAX {
        return memo[s];
    }
    let out = match &states[s] {
        SegState::Split(a, b) => {
            memo_closure(states, memo, *a as usize) | memo_closure(states, memo, *b as usize)
        }
        SegState::Jump(n) | SegState::DotGuard(n) => memo_closure(states, memo, *n as usize),
        _ => 1u64 << s,
    };
    memo[s] = out;
    out
}

/// ε-closure of `s` with DotGuard edges blocked — the entry closure
/// for a protected leading `.`. (Guard-passing closures come from
/// [`memo_closure`].)
fn closure_of_dot_blocked(states: &[SegState], start: usize) -> u64 {
    let mut seen: u64 = 0;
    let mut out: u64 = 0;
    let mut stack = [0u8; 2 * MAX_SEG_NFA_STATES];
    let mut sp = 0usize;
    stack[sp] = start as u8;
    sp += 1;
    while sp > 0 {
        sp -= 1;
        let cur = stack[sp] as usize;
        if seen & (1u64 << cur) != 0 {
            continue;
        }
        seen |= 1u64 << cur;
        match &states[cur] {
            SegState::Split(a, b) => {
                stack[sp] = *a;
                sp += 1;
                stack[sp] = *b;
                sp += 1;
            }
            SegState::Jump(n) => {
                stack[sp] = *n;
                sp += 1;
            }
            SegState::DotGuard(_) => {} // blocked
            _ => out |= 1u64 << cur,
        }
    }
    out
}

/// Does the NFA accept any string at all? Fixpoint over consuming
/// states whose byte test is satisfiable by some byte. Per-state
/// satisfiability (the 256-scan for classes) and successor closures
/// are computed once, outside the fixpoint.
fn compute_satisfiable(states: &[SegState], closures: &[u64], init: u64, accept_mask: u64) -> bool {
    let mut fire_next = [0u8; MAX_SEG_NFA_STATES];
    let mut fires: u64 = 0;
    for (s, st) in states.iter().enumerate() {
        let next = match st {
            SegState::Byte(_, n) | SegState::Any(n, _) => Some(*n),
            SegState::Class(cls, n, _) => {
                if (0u16..=255).any(|b| cls.matches(b as u8)) {
                    Some(*n)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(nx) = next {
            fires |= 1u64 << s;
            fire_next[s] = nx;
        }
    }
    let mut reach = init;
    loop {
        let mut grew = false;
        let mut bits = reach & fires;
        while bits != 0 {
            let s = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let clo = closures[fire_next[s] as usize];
            if reach | clo != reach {
                reach |= clo;
                grew = true;
            }
        }
        if !grew {
            return reach & accept_mask != 0;
        }
    }
}

struct SegBuilder {
    states: Vec<SegState>,
    tails: Vec<u8>,
    ci: bool,
}

impl SegBuilder {
    fn alloc(&mut self, s: SegState) -> Option<u8> {
        if self.states.len() >= MAX_SEG_NFA_STATES {
            return None;
        }
        self.states.push(s);
        Some((self.states.len() - 1) as u8)
    }

    fn patch(&mut self, state: u8, target: u8) {
        match &mut self.states[state as usize] {
            SegState::Byte(_, n)
            | SegState::Class(_, n, _)
            | SegState::Any(n, _)
            | SegState::Jump(n)
            | SegState::DotGuard(n) => {
                if *n == UNSET {
                    *n = target;
                }
            }
            SegState::Split(a, b) => {
                if *a == UNSET {
                    *a = target;
                }
                if *b == UNSET {
                    *b = target;
                }
            }
            SegState::Match => unreachable!(),
        }
    }

    fn compile_ops(&mut self, ops: &[Op], dot: bool) -> Option<u8> {
        if ops.is_empty() {
            let s = self.alloc(SegState::Jump(UNSET))?;
            self.tails.push(s);
            return Some(s);
        }
        let mut entry: Option<u8> = None;
        let mut pending: Vec<u8> = Vec::new();
        for op in ops {
            let (op_entry, mut op_tails) = self.compile_op(op, dot)?;
            for t in pending.drain(..) {
                self.patch(t, op_entry);
            }
            pending.append(&mut op_tails);
            if entry.is_none() {
                entry = Some(op_entry);
            }
        }
        self.tails.append(&mut pending);
        entry
    }

    fn compile_op(&mut self, op: &Op, dot: bool) -> Option<(u8, Vec<u8>)> {
        match op {
            Op::Lit(bytes) => {
                debug_assert!(!bytes.is_empty());
                let entry = self.lit_state(bytes[0])?;
                let mut prev = entry;
                for &b in &bytes[1..] {
                    let s = self.lit_state(b)?;
                    self.patch(prev, s);
                    prev = s;
                }
                Some((entry, vec![prev]))
            }
            Op::AnyChar => {
                let s = self.alloc(SegState::Any(UNSET, !dot))?;
                Some((s, vec![s]))
            }
            Op::Class(cls) => {
                let dp = !dot && cls.negated;
                let s = self.alloc(SegState::Class(Box::new(cls.clone()), UNSET, dp))?;
                Some((s, vec![s]))
            }
            Op::Star => {
                let entry = self.alloc(SegState::Split(UNSET, UNSET))?;
                let body = self.alloc(SegState::Any(entry, !dot))?;
                let exit = if !dot {
                    self.alloc(SegState::DotGuard(UNSET))?
                } else {
                    self.alloc(SegState::Jump(UNSET))?
                };
                if let SegState::Split(a, b) = &mut self.states[entry as usize] {
                    *a = body;
                    *b = exit;
                }
                Some((entry, vec![exit]))
            }
            Op::Alternation(branches) => {
                debug_assert!(!branches.is_empty());
                let mut entries = Vec::with_capacity(branches.len());
                let mut tails: Vec<u8> = Vec::new();
                for branch in branches {
                    let saved = std::mem::take(&mut self.tails);
                    let e = self.compile_ops(branch, dot)?;
                    let branch_tails = std::mem::replace(&mut self.tails, saved);
                    entries.push(e);
                    tails.extend(branch_tails);
                }
                let mut next_state: Option<u8> = None;
                for i in (0..branches.len().saturating_sub(1)).rev() {
                    let a = entries[i];
                    let b = next_state.unwrap_or(entries[i + 1]);
                    let s = self.alloc(SegState::Split(a, b))?;
                    next_state = Some(s);
                }
                Some((next_state.unwrap_or(entries[0]), tails))
            }
            // Separator-crossing ops never appear inside a segment.
            _ => None,
        }
    }

    fn lit_state(&mut self, b: u8) -> Option<u8> {
        let alt = ascii_case_alt(b);
        if self.ci && alt != b {
            let cls = CharClass {
                negated: false,
                items: vec![ClassItem::Byte(b), ClassItem::Byte(alt)],
            };
            self.alloc(SegState::Class(Box::new(cls), UNSET, false))
        } else {
            self.alloc(SegState::Byte(b, UNSET))
        }
    }
}
