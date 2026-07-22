//! Thompson NFA compiled from an [`OpProgram`]. Two consumers:
//!
//! - [`super::thompson_dfa`] eagerly subset-constructs a DFA over it
//!   for the Tier-1/2 fast path (~1-2 ns/byte at match time).
//! - [`super::pikevm::PikeVm`] simulates it directly with O(n·m)
//!   linear time and no backtracking — used as a fallback when DFA
//!   subset construction overruns its state cap.
//!
//! # Construction
//!
//! One [`Trans`] per NFA state. Compound ops (`Lit` multi-byte, segment-aware
//! `OptSegmentsSlash`/`SlashAnything`/`GlobstarAny`, brace `Alternation`) are
//! flattened into a sequence of primitive states connected by ε-transitions
//! ([`Trans::Split`] / [`Trans::Jump`]).
//!
//! # Dot protection
//!
//! A state is `dot_protected: true` when, at a segment start (byte 0, or
//! after a separator), consuming a byte that equals `.` must cause the thread
//! to die. Implements GLOB_SPEC §6 (segment-leading dot). States that
//! literally match `.` (e.g. a `Lit` state for byte `.`) are never
//! dot-protected — matching `.` is the state's sole purpose there.
//!
//! # State count
//!
//! Thompson construction has a known bound: ~2× the op count for most
//! constructions. Our segment-aware ops are slightly heavier (up to 4 states
//! per OSS/SlashAnything/GlobstarAny), so a typical pattern compiles to
//! 2-5× its op count in states.

use crate::ast::{CharClass, ClassItem};
use crate::engine::ops::{Op, OpProgram};
use crate::options::ascii_case_alt;

/// NFA state identifier. `u32` accommodates any pattern that can reasonably
/// be passed to `Glob::new` — `MAX_PATTERN_LEN` is 64 KB and each byte
/// produces at most a handful of states.
pub(crate) type StateId = u32;

/// Sentinel for "no state yet"; used during construction to patch forward
/// references. A legal `StateId` is always less than `states.len()`.
const UNSET: StateId = StateId::MAX;

/// A single NFA transition.
///
/// The order here matches both frequency-of-execution (hot variants first for
/// better branch prediction) and the mental model used in construction.
#[derive(Clone, Debug)]
pub(crate) enum Trans {
    /// Accept. Reaching this state with path exhausted (Full mode) or with
    /// any active thread here (end of run) means the pattern matched.
    Match,

    /// Consume byte `b` exactly. Never dot-protected — a literal `.` state
    /// exists precisely to match `.`.
    Byte { b: u8, next: StateId },

    /// Consume one byte matching `class`. `dot_protected` is true iff the
    /// class is negated AND `!ctx.dot`.
    Class {
        class: Box<CharClass>,
        next: StateId,
        dot_protected: bool,
    },

    /// Consume one non-separator byte. For `*` / `?` and the body of
    /// `OptSegmentsSlash`. Dot-protected under `!ctx.dot`.
    AnyNonSep { next: StateId, dot_protected: bool },

    /// Consume one byte of any kind (including separator). For the interior of
    /// `SlashAnything` and `GlobstarAny`. Dot-protected under `!ctx.dot`.
    AnyByte { next: StateId, dot_protected: bool },

    /// Consume one separator byte. Strict: a single `Sep` state matches
    /// exactly one separator byte. Run absorption (1+ for `Op::SepRun`,
    /// 0+ for `Op::LeadingSeps`) is built around this state with `Split`
    /// loops at compile time, not at byte-step time.
    Sep { next: StateId },

    /// ε-transition: nondeterministic fork to two children. Both are taken
    /// at step time (ε-closure).
    Split { a: StateId, b: StateId },

    /// ε-transition: unconditional jump. Could equivalently be
    /// `Split { a: next, b: next }` but kept distinct for clarity and a
    /// modest step-time saving (no duplicate ε-closure insert).
    Jump { next: StateId },

    /// ε-transition guarded by dot protection. Evaluated at byte-step time
    /// (not during the pre-step ε-closure) because the decision depends on
    /// the upcoming byte: if `at_segment_start && upcoming_byte == b'.'`,
    /// the thread dies; otherwise it ε-transitions to `next`. Models glob's
    /// "`*` cannot zero-match at a hidden-file segment start" rule — the
    /// same check `Backtrack` performs at the head of `Op::Star`.
    DotGuard { next: StateId },
}

/// Compiled Thompson NFA.
#[derive(Clone, Debug)]
pub struct Thompson {
    pub(crate) states: Vec<Trans>,
    pub(crate) initial: StateId,
    /// `Trans::Match` state id. Stored so [`super::pikevm::PikeVm`] can
    /// derive its own `reach_to_accept` lazily when `match_dir` is first
    /// called — the DFA path never needs this, and the typical compile
    /// stays in the DFA path, so eager computation here would be dead
    /// work for every common pattern.
    pub(crate) accept: StateId,
    /// Mask of states from which [`Trans::Match`] is reachable via **zero**
    /// byte steps, i.e. through any chain of [`Trans::Split`] /
    /// [`Trans::Jump`] / [`Trans::DotGuard`] ε-like transitions.
    ///
    /// `DotGuard` is included here because at end-of-input there is no
    /// "upcoming byte" to trip its guard — conceptually `Star`'s zero-match
    /// branch must succeed at EOF (e.g. pattern `[^.]*` on path `main.rs`:
    /// the star consumes nothing beyond the class, and the DotGuard→Match
    /// tail should finish the match). The per-step [`epsilon_closure`]
    /// deliberately does NOT expand `DotGuard` because the decision depends
    /// on the next byte; this separate mask captures the EOF semantics so
    /// callers can check acceptance without re-walking the ε graph.
    ///
    /// Used by [`super::pikevm::PikeVm::has_accept`] and
    /// [`super::thompson_dfa`] when computing DFA state acceptance.
    pub(crate) accepts_at_eof: Vec<bool>,
}

impl Thompson {
    /// Number of NFA states. Used by [`crate::Glob::union_with`] to
    /// route huge unions (`state_count > NFA_FAST_PATH_LIMIT`) through
    /// per-pattern decomposition instead of a merged DFA.
    pub(crate) fn state_count(&self) -> usize {
        self.states.len()
    }

    /// Compile the program into an NFA. Never fails — every op in
    /// [`super::ops`] has a Thompson translation.
    pub(crate) fn compile(program: &OpProgram, dot: bool) -> Self {
        let mut builder = Builder::new(program.case_insensitive());
        let initial = builder.alloc(Trans::Jump { next: UNSET });
        // `accept` is built after the body so the body's tail can patch to it.
        let body_entry = builder.compile_ops(program.ops(), dot);
        let accept = builder.alloc(Trans::Match);
        // Entry jumps into the body; the body's tail is patched to accept.
        builder.patch(initial, body_entry);
        let tails = std::mem::take(&mut builder.tail_patches);
        for st in tails {
            builder.patch(st, accept);
        }
        let states = builder.states;
        // `reach_to_accept` deliberately NOT computed here — only PikeVm
        // needs it (for `match_dir` prefix mode), and PikeVm only
        // constructs when the DFA path overflows. DFA path skips this
        // O(n+E) backward BFS entirely.
        let accepts_at_eof = compute_accepts_at_eof(&states);
        Self {
            states,
            initial,
            accept,
            accepts_at_eof,
        }
    }
}

/// Incremental NFA builder. Every `compile_*` helper returns the entry
/// state of the compiled fragment and appends all its states to
/// `self.states`. Forward refs (the "tail" of a fragment that patches to
/// its successor) are collected in `tail_patches` so the caller can attach
/// them to the correct follow-on state once known.
struct Builder {
    states: Vec<Trans>,
    /// States whose `next` / `a` / `b` field is currently `UNSET` and will
    /// be patched to the successor state after the caller decides what the
    /// successor is.
    tail_patches: Vec<StateId>,
    /// Mirror of [`OpProgram::case_insensitive`]. Drives `compile_lit` to
    /// emit a 2-item `Trans::Class` per ASCII-letter byte instead of an
    /// exact `Trans::Byte`, so pattern `Op::Lit` handles case-folded
    /// matches. Non-letter bytes still get `Trans::Byte` (case folding
    /// is a no-op on them).
    case_insensitive: bool,
}

impl Builder {
    fn new(case_insensitive: bool) -> Self {
        Self {
            states: Vec::with_capacity(32),
            tail_patches: Vec::new(),
            case_insensitive,
        }
    }

    fn alloc(&mut self, t: Trans) -> StateId {
        let id = self.states.len() as StateId;
        self.states.push(t);
        id
    }

    /// Patch every `UNSET` field of `state` to `target`. A state with more
    /// than one `UNSET` field (only `Split`) has them both patched to the
    /// same target when both were left unset by the caller — but our
    /// construction never does that; `Split` always has exactly one
    /// known destination (e.g. the loop body) and one unset tail.
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

    /// Compile a flat sequence of ops into a linear chain, returning the
    /// entry state. The final op's "next" field is left `UNSET` and added
    /// to `tail_patches` so the caller patches it to whatever follows.
    fn compile_ops(&mut self, ops: &[Op], dot: bool) -> StateId {
        if ops.is_empty() {
            // Empty body: return a pass-through Jump whose tail is patched
            // by the caller.
            let s = self.alloc(Trans::Jump { next: UNSET });
            self.tail_patches.push(s);
            return s;
        }
        let mut entry: Option<StateId> = None;
        // As we compile each op, its entry state is known; we patch the
        // PREVIOUS op's tail to this entry.
        let mut pending_tails: Vec<StateId> = Vec::new();
        for op in ops {
            let (op_entry, mut op_tails) = self.compile_op(op, dot);
            for tail in pending_tails.drain(..) {
                self.patch(tail, op_entry);
            }
            pending_tails.append(&mut op_tails);
            if entry.is_none() {
                entry = Some(op_entry);
            }
        }
        // The final op's tails are the body's tails — caller patches them.
        self.tail_patches.append(&mut pending_tails);
        entry.unwrap()
    }

    /// Compile a single op, returning (entry_state, tail_states_to_patch).
    /// The tails collectively represent "all the UNSET 'next' pointers that
    /// should be patched to the op following this one".
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
                // Raw globstar should have been folded by the lowering pass.
                // Compile to an unmatchable state so a mis-lowering is loud.
                let s = self.alloc(Trans::Byte { b: 0, next: UNSET });
                // No tail — it can never be reached since byte 0 won't appear
                // in paths. But return an unconnected tail so the chain
                // remains structurally valid.
                (s, vec![s])
            }
        }
    }

    /// Lit(b0 b1 … bk) → chain of byte-consuming states.
    ///
    /// In the default case each byte becomes a `Trans::Byte { b }` (exact
    /// match). Under `case_insensitive`, ASCII letters are compiled as a
    /// 2-item `Trans::Class` matching both cases instead — non-letter
    /// bytes still get the cheaper `Trans::Byte` since case folding is a
    /// no-op on them. Separator-containing literals don't appear here
    /// (the lowering pass emits `Op::Sep` separately), so the class's
    /// built-in separator guard never fires on these synthesized classes.
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

    /// Allocate a single-byte consuming state, honoring `case_insensitive`
    /// for ASCII letters. Factored out of [`Self::compile_lit`] so the
    /// chain-building stays readable.
    fn alloc_lit_byte(&mut self, b: u8) -> StateId {
        let alt = ascii_case_alt(b);
        if self.case_insensitive && alt != b {
            // ASCII letter under CI: emit a 2-item positive class.
            // `negated=false` → `dot_protected=false` (letters are never
            // `.` anyway, but the class API still demands the flag be set
            // consistently with non-CI Class compilation).
            let class = CharClass {
                negated: false,
                items: vec![ClassItem::Byte(b), ClassItem::Byte(alt)],
            };
            self.alloc(Trans::Class {
                class: Box::new(class),
                next: UNSET,
                dot_protected: false,
            })
        } else {
            self.alloc(Trans::Byte { b, next: UNSET })
        }
    }

    fn compile_any_non_sep(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let s = self.alloc(Trans::AnyNonSep {
            next: UNSET,
            dot_protected: !dot,
        });
        (s, vec![s])
    }

    /// Star → Split(body, dot_guard) where body consumes any non-sep byte
    /// with dot protection, and dot_guard ε-transitions to the zero-match
    /// exit but is gated by the same dot-protection rule. Mirrors
    /// `Backtrack`'s `Op::Star`: if at segment start and the next byte is
    /// `.`, the Star fails entirely (including the zero-match branch).
    fn compile_star(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → body
            b: UNSET, // → dot_guard
        });
        let body = self.alloc(Trans::AnyNonSep {
            next: entry,
            dot_protected: !dot,
        });
        // dot_guard's `next` is the zero-match exit — patched by the outer
        // compile_ops loop via tail_patches.
        let dot_guard = if !dot {
            self.alloc(Trans::DotGuard { next: UNSET })
        } else {
            // With dot protection disabled globally, the guard is a no-op —
            // skip it to keep the NFA compact.
            self.alloc(Trans::Jump { next: UNSET })
        };
        if let Trans::Split { a, b } = &mut self.states[entry as usize] {
            *a = body;
            *b = dot_guard;
        }
        (entry, vec![dot_guard])
    }

    fn compile_class(&mut self, class: &CharClass, dot: bool) -> (StateId, Vec<StateId>) {
        let s = self.alloc(Trans::Class {
            class: Box::new(class.clone()),
            next: UNSET,
            dot_protected: !dot && class.negated,
        });
        (s, vec![s])
    }

    /// Sep → requires exactly one separator byte. Strict semantics
    /// per `picomatch` / `globset` / `bash` on plain `/` patterns:
    /// redundant separator runs in the path (`a//b`) do NOT collapse
    /// against a single `/` in the pattern.
    fn compile_sep(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Sep { next: UNSET });
        (entry, vec![entry])
    }

    /// SepRun → requires one or more separator bytes (lenient).
    /// Emitted by the globstar fold for the explicit `/` adjacent to
    /// `**`, so `a/**/b` matches `a//b` — same boundary behavior as
    /// `picomatch` / `globset` / `wax`. Structure:
    ///   entry: Sep(→tail_split)
    ///   tail_split: Split(loop_body, exit)
    ///   loop_body: Sep(→tail_split)
    fn compile_sep_run(&mut self) -> (StateId, Vec<StateId>) {
        let tail_split = self.alloc(Trans::Split {
            a: UNSET, // → loop_body
            b: UNSET, // → exit (tail)
        });
        let loop_body = self.alloc(Trans::Sep { next: tail_split });
        if let Trans::Split { a, .. } = &mut self.states[tail_split as usize] {
            *a = loop_body;
        }
        let entry = self.alloc(Trans::Sep { next: tail_split });
        (entry, vec![tail_split])
    }

    /// LeadingSeps → zero-or-more separators.
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

    /// OptSegmentsSlash: matches `(<segment>/)*` — zero or more full
    /// segments each followed by a separator. Dot-protected at each
    /// segment start.
    ///
    ///   entry: Split(seg_body, exit)
    ///   seg_body: AnyNonSep(→seg_cont)
    ///   seg_cont: Split(seg_body_loop, sep_start)
    ///   seg_body_loop: AnyNonSep(→seg_cont)   (same as seg_body, but no dot protection)
    ///   sep_start: Sep(→sep_tail)
    ///   sep_tail: Split(sep_loop, entry)      (return to entry for next segment)
    ///   sep_loop: Sep(→sep_tail)
    fn compile_oss(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → seg_body
            b: UNSET, // → exit
        });
        // segment body entry is dot-protected (segment start)
        let seg_body = self.alloc(Trans::AnyNonSep {
            next: UNSET,
            dot_protected: !dot,
        });
        // after the first byte of the segment, further bytes are not
        // dot-protected (we're past the segment start)
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
            a: UNSET, // → sep_loop (collapse consecutive)
            b: UNSET, // → entry (start next segment)
        });
        let sep_loop = self.alloc(Trans::Sep { next: sep_tail });

        // Wire segment body.
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
            *a = sep_loop;
            *b = entry;
        }
        if let Trans::Split { a, .. } = &mut self.states[entry as usize] {
            *a = seg_body;
        }
        (entry, vec![entry])
    }

    /// SlashAnything: requires one separator, then absorbs any bytes with
    /// dot protection at segment boundaries.
    ///
    ///   entry: Sep(→post_sep)
    ///   post_sep: Split(sep_loop, tail)
    ///   sep_loop: Sep(→post_sep)
    ///   tail: Split(tail_loop, exit)
    ///   tail_loop: AnyByte(→tail)   (dot_protected at segment start; we track
    ///                                 that dynamically — the simpler model is
    ///                                 to always flag dot_protected and have
    ///                                 the VM only enforce at segment start)
    fn compile_slash_anything(&mut self, dot: bool) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Sep { next: UNSET });
        let post_sep = self.alloc(Trans::Split {
            a: UNSET, // → sep_loop
            b: UNSET, // → tail
        });
        let sep_loop = self.alloc(Trans::Sep { next: post_sep });
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
            *a = sep_loop;
            *b = tail;
        }
        if let Trans::Split { a, .. } = &mut self.states[tail as usize] {
            *a = tail_loop;
        }
        (entry, vec![tail])
    }

    /// GlobstarAny: absorbs any bytes (including separators), dot-protected
    /// at segment boundaries. Can also match empty.
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

    /// Alternation: fork to each branch, each branch's tail joins to the
    /// alternation's exit.
    ///
    /// Shape for N branches:
    ///   entry (N-1 chained Splits, each fanning off one branch)
    ///   each branch compiled as a sub-sequence; its tails are collected
    ///   and returned as the alternation's tails.
    fn compile_alternation(&mut self, branches: &[Vec<Op>], dot: bool) -> (StateId, Vec<StateId>) {
        debug_assert!(!branches.is_empty());
        if branches.len() == 1 {
            return (
                self.compile_ops(&branches[0], dot),
                std::mem::take(&mut self.tail_patches),
            );
        }
        // Build entry Splits fanning out to each branch's entry.
        //   s0 = Split(branch0, s1)
        //   s1 = Split(branch1, s2)
        //   ...
        //   s_{N-2} = Split(branch_{N-2}, branch_{N-1})
        // Branches are compiled first so we have their entry ids, then the
        // Splits are allocated referring to them.
        let mut branch_entries = Vec::with_capacity(branches.len());
        let mut branch_tails = Vec::new();
        for branch in branches {
            let entry = self.compile_ops(branch, dot);
            let tails = std::mem::take(&mut self.tail_patches);
            branch_entries.push(entry);
            branch_tails.extend(tails);
        }
        // Chain of Splits.
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

/// Forward fixpoint: `accepts_at_eof[s] = true` iff [`Trans::Match`] is
/// reachable from `s` via zero byte-consuming transitions, traversing
/// [`Trans::Split`] / [`Trans::Jump`] / [`Trans::DotGuard`] as ε.
///
/// See [`Thompson::accepts_at_eof`] for why `DotGuard` is treated as ε
/// here but not in [`epsilon_closure`]'s per-step version.
fn compute_accepts_at_eof(states: &[Trans]) -> Vec<bool> {
    let n = states.len();
    let mut acc = vec![false; n];
    for (i, t) in states.iter().enumerate() {
        if matches!(t, Trans::Match) {
            acc[i] = true;
        }
    }
    // Fixpoint over ε-like predecessors. Iteration bound is loose but the
    // NFA is tiny (a few hundred states at most) — no perf concern.
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

/// Reverse BFS from `accept`, marking every state that can reach it via a
/// **non-empty** transition sequence. `reach_to_accept[accept]` is
/// deliberately `false`: Match itself contributes nothing to Prefix-mode
/// descent (it has no outgoing transitions), and leaving its flag set
/// would cause `match_dir` to return `DescendAndMatch` when the only
/// active thread is Match — wrong, because a descendant can't extend an
/// already-complete match.
pub(crate) fn compute_reach_to_accept(states: &[Trans], accept: StateId) -> Vec<bool> {
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
    // Seed with direct predecessors of `accept` so `reach[accept]` stays
    // false. Any state whose outgoing transition (byte or ε) lands on
    // `accept` needs the flag.
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

/// Pre-compute per-state ε-closure bitmaps (Split/Jump only).
/// `result[s * n_words .. (s+1) * n_words]` = bits of leaf states
/// reachable from `s` via Split/Jump. Leaves include byte-consumers,
/// `DotGuard`, and `Match` — anything that's NOT a Split/Jump.
///
/// Shared by [`super::pikevm::PikeVm`] (per-byte ε expansion replaced
/// by `O(active × n_words)` bitmap ORs) and
/// [`super::thompson_dfa::ThompsonDfa`] (subset construction, same
/// avoid-recursive-BFS trick).
///
/// Implementation: two-phase post-order DFS via explicit stack.
/// Each work item is `(state, phase)` packed into a `u32` — the
/// high bit signals "exit phase, fold children's closures into
/// mine". The plain recursive form bottomed out on deep brace
/// alternations (the merged-pattern union of 200+ branches builds
/// a Split tree several hundred deep), so we keep state on the heap
/// rather than the call stack.
pub(crate) fn compute_static_closures(thompson: &Thompson, n_words: usize) -> Vec<u64> {
    let n = thompson.states.len();
    let mut closures = vec![0u64; n * n_words];
    let mut seen = vec![false; n];

    /// Top bit of the work-item word. Set ⇒ "exit phase" (children
    /// already processed, fold their closures); clear ⇒ "enter phase".
    /// Safe because Thompson construction caps `StateId` well below
    /// `1 << 31` (`MAX_PATTERN_LEN` is 64 KB and each byte yields
    /// only a handful of states).
    const EXIT_BIT: u32 = 1 << 31;
    let mut stack: Vec<u32> = Vec::new();

    for root in 0..n {
        if seen[root] {
            continue;
        }
        stack.push(root as u32);

        while let Some(item) = stack.pop() {
            // Exit phase — children's closures are final, fold them.
            if item & EXIT_BIT != 0 {
                let s = (item & !EXIT_BIT) as usize;
                let s_base = s * n_words;
                match &thompson.states[s] {
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
                        // Leaf: closure(s) = {s}.
                        closures[s_base + (s >> 6)] = 1u64 << (s & 63);
                    }
                }
                continue;
            }

            // Enter phase — first time we visit this state.
            let s = item as usize;
            if seen[s] {
                continue;
            }
            seen[s] = true;

            // LIFO order: push exit marker first so it pops AFTER
            // children's exits.
            stack.push((s as u32) | EXIT_BIT);

            match &thompson.states[s] {
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
                _ => {} // leaf — exit phase fills the bitmap
            }
        }
    }

    closures
}
