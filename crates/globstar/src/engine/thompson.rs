//! Thompson NFA compiled from an [`OpProgram`] and run by
//! [`super::pikevm::PikeVm`] in linear time with no backtracking. The
//! fallback when the segment engine can't represent a pattern.
//!
//! One [`Trans`] per state. Compound ops become a chain of primitive states
//! joined by ε-transitions ([`Trans::Split`], [`Trans::Jump`]).
//!
//! A `dot_protected` state refuses a `.` at a segment start (byte 0, or right
//! after a separator). This is glob's segment-leading-dot rule (GLOB_SPEC §6).
//! A state whose job is to match `.` is never dot-protected.

use crate::ast::CharClass;
use crate::engine::ops::{Op, OpProgram};

pub(crate) type StateId = u32;

/// "No state yet", used to patch forward references during construction.
const UNSET: StateId = StateId::MAX;

/// One NFA transition. Ordered hot-first for branch prediction.
#[derive(Clone, Debug)]
pub(crate) enum Trans {
    /// Accept state. A thread here at end of input means a match.
    Match,

    /// Consume byte `b`.
    Byte { b: u8, next: StateId },

    /// Consume one byte in `class`. `dot_protected` when the class is negated
    /// and dot mode is off.
    Class {
        class: Box<CharClass>,
        next: StateId,
        dot_protected: bool,
    },

    /// Consume one non-separator byte (`*`, `?`, and the OSS body).
    AnyNonSep { next: StateId, dot_protected: bool },

    /// Consume any byte, separators included (inside `SlashAnything` and
    /// `GlobstarAny`).
    AnyByte { next: StateId, dot_protected: bool },

    /// Consume exactly one separator. Runs (`SepRun`, `LeadingSeps`) are built
    /// from `Split` loops around this state, not handled per byte.
    Sep { next: StateId },

    /// ε fork. Both targets are taken during ε-closure.
    Split { a: StateId, b: StateId },

    /// ε jump to `next`.
    Jump { next: StateId },

    /// ε jump that instead dies when the thread is at a segment start and the
    /// next byte is `.`, so `*` can't zero-match a hidden file. Checked at step
    /// time because it depends on the next byte.
    DotGuard { next: StateId },
}

/// Compiled Thompson NFA.
#[derive(Clone, Debug)]
pub(crate) struct Thompson {
    pub(crate) states: Vec<Trans>,
    pub(crate) initial: StateId,
    pub(crate) accept: StateId,

    /// States that reach [`Trans::Match`] through ε-edges alone, with no more
    /// bytes to read.
    ///
    /// `DotGuard` counts as ε here. At end of input there is no next byte to
    /// trip its guard, so `*`'s zero-match branch must still accept (`[^.]*`
    /// on `main.rs`). The per-step closure skips `DotGuard`, so this mask
    /// covers the end-of-input case on its own.
    pub(crate) accepts_at_eof: Vec<bool>,
}

impl Thompson {
    /// Compile the program into an NFA. Always succeeds.
    pub(crate) fn compile(program: &OpProgram, dot: bool) -> Self {
        let mut builder = Builder::new(program.case_insensitive);
        let (initial, tails) = builder.compile_ops(&program.ops, dot);
        let accept = builder.alloc(Trans::Match);
        for st in tails {
            builder.patch(st, accept);
        }
        let states = builder.states;
        let accepts_at_eof = compute_accepts_at_eof(&states);
        Self {
            states,
            initial,
            accept,
            accepts_at_eof,
        }
    }
}

/// Builds the NFA one op at a time. Each `compile_*` returns the fragment's
/// entry state and its dangling exits for the caller to wire onward.
struct Builder {
    states: Vec<Trans>,
    /// When set, a `Lit` byte that is an ASCII letter compiles to a two-item
    /// `Class` matching both cases instead of an exact `Byte`.
    case_insensitive: bool,
}

impl Builder {
    fn new(case_insensitive: bool) -> Self {
        Self {
            states: Vec::with_capacity(32),
            case_insensitive,
        }
    }

    fn alloc(&mut self, t: Trans) -> StateId {
        let id = self.states.len() as StateId;
        self.states.push(t);
        id
    }

    /// Point every `UNSET` field of `state` at `target`.
    fn patch(&mut self, state: StateId, target: StateId) {
        match &mut self.states[state as usize] {
            Trans::Match => panic!("cannot patch a Match state"),
            Trans::Byte { next, .. }
            | Trans::Class { next, .. }
            | Trans::AnyNonSep { next, .. }
            | Trans::AnyByte { next, .. }
            | Trans::Sep { next, .. }
            | Trans::Jump { next }
            | Trans::DotGuard { next } => {
                if *next == UNSET {
                    *next = target;
                }
            }
            Trans::Split { a, b } => {
                if *a == UNSET {
                    *a = target;
                }
                if *b == UNSET {
                    *b = target;
                }
            }
        }
    }

    /// Compile a sequence of ops into a chain, returning its entry state and
    /// the last op's dangling exits.
    fn compile_ops(&mut self, ops: &[Op], dot: bool) -> (StateId, Vec<StateId>) {
        if ops.is_empty() {
            let s = self.alloc(Trans::Jump { next: UNSET });
            return (s, vec![s]);
        }
        let mut entry: Option<StateId> = None;
        let mut pending_tails: Vec<StateId> = Vec::new();
        for op in ops {
            let (op_entry, mut op_tails) = self.compile_op(op, dot);
            // Wire the previous op's exits onto this op's entry.
            for tail in pending_tails.drain(..) {
                self.patch(tail, op_entry);
            }
            pending_tails.append(&mut op_tails);
            if entry.is_none() {
                entry = Some(op_entry);
            }
        }
        (entry.unwrap(), pending_tails)
    }

    /// Compile one op. Returns its entry state and the exits to wire onward.
    fn compile_op(&mut self, op: &Op, dot: bool) -> (StateId, Vec<StateId>) {
        match op {
            Op::Lit(bytes) => self.compile_lit(bytes),
            Op::AnyChar => self.compile_any_non_sep(dot),
            Op::Star => self.compile_star(dot),
            Op::Class(class) => self.compile_class(class, dot),
            Op::Sep => self.compile_sep(),
            Op::SepRun => self.compile_sep_run(),
            Op::LeadingSeps => self.compile_leading_seps(),
            Op::OptSegmentsSlash => self.compile_oss(dot),
            Op::SlashAnything => self.compile_slash_anything(dot),
            Op::GlobstarAny => self.compile_globstar_any(dot),
            Op::Alternation(branches) => self.compile_alternation(branches, dot),
            Op::Globstar => {
                // Lowering folds raw globstars away; is_normalized asserts it
                // in debug builds. As a release safety net, emit a truly dead
                // state (an empty class matches no byte) so a leak can never
                // produce a false match.
                let s = self.alloc(Trans::Class {
                    class: Box::new(CharClass {
                        negated: false,
                        items: Vec::new(),
                    }),
                    next: UNSET,
                    dot_protected: false,
                });
                (s, vec![s])
            }
        }
    }

    /// One byte-consuming state per byte, chained.
    fn compile_lit(&mut self, bytes: &[u8]) -> (StateId, Vec<StateId>) {
        debug_assert!(
            !bytes.is_empty(),
            "empty Lit should not exist post-lowering"
        );
        let entry = self.alloc_lit_byte(bytes[0]);
        let mut prev = entry;
        for &b in &bytes[1..] {
            let s = self.alloc_lit_byte(b);
            self.patch(prev, s);
            prev = s;
        }
        (entry, vec![prev])
    }

    fn alloc_lit_byte(&mut self, b: u8) -> StateId {
        let class = self
            .case_insensitive
            .then(|| CharClass::ci_letter(b))
            .flatten();
        match class {
            Some(class) => self.alloc(Trans::Class {
                class: Box::new(class),
                next: UNSET,
                dot_protected: false,
            }),
            None => self.alloc(Trans::Byte { b, next: UNSET }),
        }
    }

    fn compile_any_non_sep(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let s = self.alloc(Trans::AnyNonSep {
            next: UNSET,
            dot_protected: !dot,
        });
        (s, vec![s])
    }

    /// `Split(body, exit)`. The body loops back to consume more bytes; the
    /// exit is the zero-match branch. Under dot protection the exit goes
    /// through a `DotGuard` that dies on a segment-start `.`; otherwise the
    /// Split's own dangling exit is the fragment tail.
    fn compile_star(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → body
            b: UNSET, // → dot_guard or exit
        });
        let body = self.alloc(Trans::AnyNonSep {
            next: entry,
            dot_protected: !dot,
        });
        if let Trans::Split { a, .. } = &mut self.states[entry as usize] {
            *a = body;
        }
        if !dot {
            let dot_guard = self.alloc(Trans::DotGuard { next: UNSET });
            if let Trans::Split { b, .. } = &mut self.states[entry as usize] {
                *b = dot_guard;
            }
            (entry, vec![dot_guard])
        } else {
            (entry, vec![entry])
        }
    }

    fn compile_class(&mut self, class: &CharClass, dot: bool) -> (StateId, Vec<StateId>) {
        let s = self.alloc(Trans::Class {
            class: Box::new(class.clone()),
            next: UNSET,
            dot_protected: !dot && class.negated,
        });
        (s, vec![s])
    }

    /// One separator, matched strictly. A single `/` in the pattern does not
    /// absorb a run like `a//b` in the path (matches picomatch, globset, bash).
    fn compile_sep(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Sep { next: UNSET });
        (entry, vec![entry])
    }

    /// One or more separators. Emitted for the `/` next to a `**`, so
    /// `a/**/b` matches `a//b`.
    ///
    ///   entry: Sep(→tail_split)
    ///   tail_split: Split(entry, exit)
    fn compile_sep_run(&mut self) -> (StateId, Vec<StateId>) {
        let tail_split = self.alloc(Trans::Split {
            a: UNSET, // → entry (loop)
            b: UNSET, // → exit (tail)
        });
        let entry = self.alloc(Trans::Sep { next: tail_split });
        if let Trans::Split { a, .. } = &mut self.states[tail_split as usize] {
            *a = entry;
        }
        (entry, vec![tail_split])
    }

    /// Zero or more separators (pattern-head `**/`).
    ///
    ///   entry: Split(loop_body, exit)
    ///   loop_body: Sep(→entry)
    fn compile_leading_seps(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → loop_body
            b: UNSET, // → exit
        });
        let loop_body = self.alloc(Trans::Sep { next: entry });
        if let Trans::Split { a, .. } = &mut self.states[entry as usize] {
            *a = loop_body;
        }
        (entry, vec![entry])
    }

    /// `(<segment>/)*`, zero or more whole segments each ending in a separator.
    /// Each segment start is dot-protected.
    ///
    ///   entry: Split(seg_body, exit)
    ///   seg_body: AnyNonSep(→seg_cont)
    ///   seg_cont: Split(seg_body_loop, sep_start)
    ///   seg_body_loop: AnyNonSep(→seg_cont)
    ///   sep_start: Sep(→sep_tail)
    ///   sep_tail: Split(sep_start, entry)
    fn compile_oss(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → seg_body
            b: UNSET, // → exit
        });
        // Dot-protected at the segment start.
        let seg_body = self.alloc(Trans::AnyNonSep {
            next: UNSET,
            dot_protected: !dot,
        });
        // Past the segment start, so no dot protection.
        let seg_cont = self.alloc(Trans::Split {
            a: UNSET, // → seg_body_loop (more non-sep bytes)
            b: UNSET, // → sep_start (end of segment)
        });
        let seg_body_loop = self.alloc(Trans::AnyNonSep {
            next: seg_cont,
            dot_protected: false,
        });
        let sep_start = self.alloc(Trans::Sep { next: UNSET });
        let sep_tail = self.alloc(Trans::Split {
            a: UNSET, // → sep_start (collapse consecutive)
            b: UNSET, // → entry (start next segment)
        });

        if let Trans::AnyNonSep { next, .. } = &mut self.states[seg_body as usize] {
            *next = seg_cont;
        }
        if let Trans::Split { a, b } = &mut self.states[seg_cont as usize] {
            *a = seg_body_loop;
            *b = sep_start;
        }
        if let Trans::Sep { next } = &mut self.states[sep_start as usize] {
            *next = sep_tail;
        }
        if let Trans::Split { a, b } = &mut self.states[sep_tail as usize] {
            *a = sep_start;
            *b = entry;
        }
        if let Trans::Split { a, .. } = &mut self.states[entry as usize] {
            *a = seg_body;
        }
        (entry, vec![entry])
    }

    /// One separator, then any bytes. Dot-protected at each segment start.
    ///
    ///   entry: Sep(→post_sep)
    ///   post_sep: Split(entry, tail)
    ///   tail: Split(tail_loop, exit)
    ///   tail_loop: AnyByte(→tail)
    fn compile_slash_anything(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Sep { next: UNSET });
        let post_sep = self.alloc(Trans::Split {
            a: UNSET, // → entry (collapse consecutive seps)
            b: UNSET, // → tail
        });
        let tail = self.alloc(Trans::Split {
            a: UNSET, // → tail_loop (more bytes)
            b: UNSET, // → exit
        });
        let tail_loop = self.alloc(Trans::AnyByte {
            next: tail,
            dot_protected: !dot,
        });

        if let Trans::Sep { next } = &mut self.states[entry as usize] {
            *next = post_sep;
        }
        if let Trans::Split { a, b } = &mut self.states[post_sep as usize] {
            *a = entry;
            *b = tail;
        }
        if let Trans::Split { a, .. } = &mut self.states[tail as usize] {
            *a = tail_loop;
        }
        (entry, vec![tail])
    }

    /// Any bytes including separators, or nothing. Dot-protected at each
    /// segment start.
    ///
    ///   entry: Split(body, exit)
    ///   body: AnyByte(→entry)
    fn compile_globstar_any(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → body
            b: UNSET, // → exit
        });
        let body = self.alloc(Trans::AnyByte {
            next: entry,
            dot_protected: !dot,
        });
        if let Trans::Split { a, .. } = &mut self.states[entry as usize] {
            *a = body;
        }
        (entry, vec![entry])
    }

    /// A chain of Splits fanning out to each branch, all branch exits returned
    /// together.
    fn compile_alternation(&mut self, branches: &[Vec<Op>], dot: bool) -> (StateId, Vec<StateId>) {
        debug_assert!(!branches.is_empty());
        if branches.len() == 1 {
            return self.compile_ops(&branches[0], dot);
        }
        // Compile the branches first so the Splits can point at their entries.
        let mut branch_entries = Vec::with_capacity(branches.len());
        let mut branch_tails = Vec::new();
        for branch in branches {
            let (entry, tails) = self.compile_ops(branch, dot);
            branch_entries.push(entry);
            branch_tails.extend(tails);
        }
        let mut next_state: Option<StateId> = None;
        for i in (0..branches.len() - 1).rev() {
            let a = branch_entries[i];
            let b = if let Some(n) = next_state {
                n
            } else {
                branch_entries[i + 1]
            };
            let s = self.alloc(Trans::Split { a, b });
            next_state = Some(s);
        }
        let entry = next_state.expect("at least 2 branches => at least 1 split");
        (entry, branch_tails)
    }
}

/// `accepts_at_eof[s]` is true when `s` reaches [`Trans::Match`] through
/// ε-edges alone (Split, Jump, DotGuard). See [`Thompson::accepts_at_eof`]
/// for why DotGuard counts as ε here.
fn compute_accepts_at_eof(states: &[Trans]) -> Vec<bool> {
    let n = states.len();
    let mut acc = vec![false; n];
    for (i, t) in states.iter().enumerate() {
        if matches!(t, Trans::Match) {
            acc[i] = true;
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (i, t) in states.iter().enumerate() {
            if acc[i] {
                continue;
            }
            let reaches = match t {
                Trans::Jump { next } | Trans::DotGuard { next } => acc[*next as usize],
                Trans::Split { a, b } => acc[*a as usize] || acc[*b as usize],
                _ => false,
            };
            if reaches {
                acc[i] = true;
                changed = true;
            }
        }
    }
    acc
}

impl Thompson {
    /// Reverse reachability to `accept` following at least one edge. ε edges
    /// count too, so a `DotGuard` jumping straight to `accept` is reachable
    /// without consuming a byte (harmless in practice: a guard only enters
    /// the active set alongside its star's byte-consuming body).
    ///
    /// `reach_to_accept[accept]` stays false on purpose. A lone active Match
    /// has no descendants that could extend it, so leaving its flag set would
    /// make `match_dir` wrongly report `DescendAndMatch`.
    pub(crate) fn reach_to_accept(&self) -> Vec<bool> {
        let states = &self.states;
        let accept = self.accept;
        let n = states.len();
        let mut rev: Vec<Vec<StateId>> = vec![Vec::new(); n];
        for (from, trans) in states.iter().enumerate() {
            let from = from as StateId;
            match trans {
                Trans::Match => {}
                Trans::Byte { next, .. }
                | Trans::Class { next, .. }
                | Trans::AnyNonSep { next, .. }
                | Trans::AnyByte { next, .. }
                | Trans::Sep { next, .. }
                | Trans::Jump { next }
                | Trans::DotGuard { next } => {
                    if (*next as usize) < n {
                        rev[*next as usize].push(from);
                    }
                }
                Trans::Split { a, b } => {
                    if (*a as usize) < n {
                        rev[*a as usize].push(from);
                    }
                    if (*b as usize) < n {
                        rev[*b as usize].push(from);
                    }
                }
            }
        }
        let mut reach = vec![false; n];
        let mut stack = Vec::with_capacity(n);
        // Start from `accept`'s direct predecessors so `reach[accept]` stays false.
        for &prev in &rev[accept as usize] {
            if !reach[prev as usize] {
                reach[prev as usize] = true;
                stack.push(prev);
            }
        }
        while let Some(s) = stack.pop() {
            for &prev in &rev[s as usize] {
                if !reach[prev as usize] {
                    reach[prev as usize] = true;
                    stack.push(prev);
                }
            }
        }
        reach
    }

    /// Per-state ε-closure bitmaps over Split/Jump edges, used by the Pike VM
    /// to expand ε-moves as bitmap ORs instead of a per-byte graph walk.
    /// `result[s * n_words .. (s+1) * n_words]` holds the leaf states
    /// reachable from `s`.
    ///
    /// Post-order DFS on an explicit stack. A recursive version overflowed on
    /// deeply nested brace unions.
    pub(crate) fn static_closures(&self, n_words: usize) -> Vec<u64> {
        let n = self.states.len();
        let mut closures = vec![0u64; n * n_words];
        let mut seen = vec![false; n];

        // High bit marks the exit phase (children done, fold their closures).
        const EXIT_BIT: u32 = 1 << 31;
        let mut stack: Vec<u32> = Vec::new();

        for root in 0..n {
            if seen[root] {
                continue;
            }
            stack.push(root as u32);

            while let Some(item) = stack.pop() {
                if item & EXIT_BIT != 0 {
                    let s = (item & !EXIT_BIT) as usize;
                    let s_base = s * n_words;
                    match &self.states[s] {
                        Trans::Split { a, b } => {
                            let a_base = (*a as usize) * n_words;
                            let b_base = (*b as usize) * n_words;
                            for j in 0..n_words {
                                closures[s_base + j] = closures[a_base + j] | closures[b_base + j];
                            }
                        }
                        Trans::Jump { next } => {
                            let n_base = (*next as usize) * n_words;
                            closures.copy_within(n_base..n_base + n_words, s_base);
                        }
                        _ => {
                            // Leaf: its closure is just itself.
                            closures[s_base + (s >> 6)] = 1u64 << (s & 63);
                        }
                    }
                    continue;
                }

                let s = item as usize;
                if seen[s] {
                    continue;
                }
                seen[s] = true;

                // Push the exit marker before the children so it pops after them.
                stack.push((s as u32) | EXIT_BIT);

                match &self.states[s] {
                    Trans::Split { a, b } => {
                        if !seen[*a as usize] {
                            stack.push(*a);
                        }
                        if !seen[*b as usize] {
                            stack.push(*b);
                        }
                    }
                    Trans::Jump { next } => {
                        if !seen[*next as usize] {
                            stack.push(*next);
                        }
                    }
                    _ => {}
                }
            }
        }

        closures
    }
}
