use crate::ast::CharClass;
use crate::engine::ops::Op;

const MAX_SEG_NFA_STATES: usize = 64;

const UNSET: u8 = u8::MAX;

#[derive(Debug, Clone)]
pub(super) struct SegNfa {
    states: Box<[SegState]>,

    /// ε-closure of the entry with DotGuards passable (used for all
    /// non-dot-led segments, and for the EOF/empty-segment accept).
    init: u64,

    /// Entry ε-closure for a protected leading `.`: DotGuards blocked
    /// and dot-protected consumers (`Any`, negated classes) dropped.
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
}

#[derive(Debug, Clone)]
enum SegState {
    /// byte, next
    Byte(u8, u8),
    /// class, next
    Class(Box<CharClass>, u8),
    /// next — `?` and star bodies (segments contain no separators, so
    /// "any byte" ≡ "any non-separator byte" here).
    Any(u8),
    Split(u8, u8),
    Jump(u8),
    DotGuard(u8),
    Match,
}

impl SegNfa {
    pub(super) fn compile(ops: &[Op], dot: bool, ci: bool) -> Option<Box<Self>> {
        let mut b = SegBuilder {
            states: Vec::with_capacity(16),
            tails: Vec::new(),
            ci,
        };
        let entry = b.compile_ops(ops)?;
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
            memo_closure(&states, &mut closures, s, false);
        }
        let init = closures[entry as usize];
        // Under `dot` a leading `.` needs no protection, so the blocked
        // closure is just `init` (DotGuards behave like Jumps).
        let init_dot_blocked = if dot {
            init
        } else {
            let mut blocked = [u64::MAX; MAX_SEG_NFA_STATES];
            memo_closure(&states, &mut blocked, entry as usize, true)
        };
        let accept_mask = 1u64 << (n - 1);

        // wild_led: can any state of the protected entry set consume a
        // leading `.`? If not, a dot-led segment can never match under
        // dot=false and the whole matcher is protected.
        let mut can_lit_dot = false;
        let mut bits = init_dot_blocked;
        while bits != 0 {
            let s = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            match &states[s] {
                SegState::Byte(x, _) => can_lit_dot |= *x == b'.',
                SegState::Class(cls, _) => can_lit_dot |= cls.matches(b'.'),
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
        }))
    }

    pub(super) fn matches(&self, seg: &[u8]) -> bool {
        // Under dot=true the blocked closure equals `init`, so the compile
        // already collapsed the dot decision into which set this picks.
        let protected_start = !seg.is_empty() && seg[0] == b'.';
        let mut active = if protected_start {
            self.init_dot_blocked
        } else {
            self.init
        };
        for &c in seg {
            if active == 0 {
                return false;
            }
            let mut next: u64 = 0;
            let mut bits = active;
            while bits != 0 {
                let s = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                match &self.states[s] {
                    SegState::Byte(b, nx) => {
                        if *b == c {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    SegState::Class(cls, nx) => {
                        if cls.matches(c) {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    SegState::Any(nx) => {
                        next |= self.closures[*nx as usize];
                    }
                    _ => {}
                }
            }
            active = next;
        }
        active & self.accept_mask != 0
    }
}

fn memo_closure(states: &[SegState], memo: &mut [u64], s: usize, block: bool) -> u64 {
    if memo[s] != u64::MAX {
        return memo[s];
    }
    let out = match &states[s] {
        SegState::Split(a, b) => {
            memo_closure(states, memo, *a as usize, block)
                | memo_closure(states, memo, *b as usize, block)
        }
        SegState::Jump(n) => memo_closure(states, memo, *n as usize, block),
        SegState::DotGuard(n) if !block => memo_closure(states, memo, *n as usize, block),
        SegState::DotGuard(_) => 0,
        SegState::Any(_) if block => 0,
        SegState::Class(cls, _) if block && cls.negated => 0,
        _ => 1u64 << s,
    };
    memo[s] = out;
    out
}

fn compute_satisfiable(states: &[SegState], closures: &[u64], init: u64, accept_mask: u64) -> bool {
    let mut fire_next = [0u8; MAX_SEG_NFA_STATES];
    let mut fires: u64 = 0;
    for (s, st) in states.iter().enumerate() {
        let next = match st {
            SegState::Byte(_, n) | SegState::Any(n) => Some(*n),
            SegState::Class(cls, n) => {
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
    let mut work = reach & fires;
    while work != 0 {
        let s = work.trailing_zeros() as usize;
        work &= work - 1;
        let new = closures[fire_next[s] as usize] & !reach;
        if new != 0 {
            reach |= new;
            work |= new & fires;
        }
    }
    reach & accept_mask != 0
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
            | SegState::Class(_, n)
            | SegState::Any(n)
            | SegState::Jump(n)
            | SegState::DotGuard(n) => *n = target,
            // A Split is never a dangling tail: Star returns its exit state
            // and Alternation returns its branches' leaf tails.
            SegState::Split(..) | SegState::Match => unreachable!(),
        }
    }

    fn compile_ops(&mut self, ops: &[Op]) -> Option<u8> {
        if ops.is_empty() {
            let s = self.alloc(SegState::Jump(UNSET))?;
            self.tails.push(s);
            return Some(s);
        }
        let mut entry: Option<u8> = None;
        let mut pending: Vec<u8> = Vec::new();
        for op in ops {
            let (op_entry, mut op_tails) = self.compile_op(op)?;
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

    fn compile_op(&mut self, op: &Op) -> Option<(u8, Vec<u8>)> {
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
                let s = self.alloc(SegState::Any(UNSET))?;
                Some((s, vec![s]))
            }
            Op::Class(cls) => {
                let s = self.alloc(SegState::Class(Box::new(cls.clone()), UNSET))?;
                Some((s, vec![s]))
            }
            Op::Star => {
                let entry = self.alloc(SegState::Split(UNSET, UNSET))?;
                let body = self.alloc(SegState::Any(entry))?;
                // Under `dot` the guard is inert (blocked closures are
                // never built), so DotGuard serves both compiles.
                let exit = self.alloc(SegState::DotGuard(UNSET))?;
                if let SegState::Split(a, b) = &mut self.states[entry as usize] {
                    *a = body;
                    *b = exit;
                }
                Some((entry, vec![exit]))
            }
            Op::Alternation(branches) => {
                debug_assert!(!branches.is_empty());
                // `self.tails` is empty here (every caller drains it),
                // so the branch tails can be collected straight out of it.
                let mut entries = Vec::with_capacity(branches.len());
                let mut tails: Vec<u8> = Vec::new();
                for branch in branches {
                    entries.push(self.compile_ops(branch)?);
                    tails.append(&mut self.tails);
                }
                let mut chain = entries[branches.len() - 1];
                for i in (0..branches.len() - 1).rev() {
                    chain = self.alloc(SegState::Split(entries[i], chain))?;
                }
                Some((chain, tails))
            }
            // Separator-crossing ops never appear inside a segment.
            _ => None,
        }
    }

    fn lit_state(&mut self, b: u8) -> Option<u8> {
        let class = self.ci.then(|| CharClass::ci_letter(b)).flatten();
        match class {
            Some(cls) => self.alloc(SegState::Class(Box::new(cls), UNSET)),
            None => self.alloc(SegState::Byte(b, UNSET)),
        }
    }
}
