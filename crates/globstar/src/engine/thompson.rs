use crate::ast::CharClass;
use crate::engine::ops::{Op, OpProgram};

pub(crate) type StateId = u32;

const UNSET: StateId = StateId::MAX;

#[derive(Clone, Debug)]
pub(crate) enum Trans {
    /// Accept state. A thread here at end of input means a match.
    Match,
    /// Consume byte `b`.
    Byte { b: u8, next: StateId },
    /// Consume one byte in `class`.
    Class {
        class: Box<CharClass>,
        next: StateId,
    },
    /// Consume one non-separator byte.
    AnyNonSep { next: StateId },
    /// Consume any byte, separators included.
    AnyByte { next: StateId },
    /// Consume exactly one separator.
    Sep { next: StateId },
    /// ε fork. Both targets are taken during ε-closure.
    Split { a: StateId, b: StateId },
    /// ε jump to `next`.
    Jump { next: StateId },
    /// ε jump that dies when the thread is at a segment start and the next
    /// byte is `.`. Guards `*`'s zero-match exit under `dot=false`.
    DotGuard { next: StateId },
}

#[derive(Debug)]
pub(crate) struct Thompson {
    pub(crate) states: Vec<Trans>,
    pub(crate) initial: StateId,
    pub(crate) accept: StateId,
}

impl Thompson {
    pub(crate) fn compile(program: &OpProgram, dot: bool) -> Self {
        let mut builder = Builder::new(program.case_insensitive, dot);
        let (initial, tails) = builder.compile_ops(&program.ops);
        let accept = builder.alloc(Trans::Match);
        for st in tails {
            builder.patch(st, accept);
        }
        Self {
            states: builder.states,
            initial,
            accept,
        }
    }

    /// Per-state ε-closure bitmaps. `result[s * n_words .. (s+1) * n_words]`
    /// holds the leaf states reachable from `s` over Split/Jump edges.
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
                    Trans::Jump { next } if !seen[*next as usize] => {
                        stack.push(*next);
                    }
                    _ => {}
                }
            }
        }

        closures
    }

    /// Packed [`Trans::DotGuard`] expansions, one record of `1 + n_words`
    /// words per guard. A record holds the guard's state id, then the bitmap
    /// it releases when it passes. Chained guards are folded in, so the run
    /// loop expands every active guard in a single pass.
    pub(crate) fn guard_expansions(&self, closures: &[u64], n_words: usize) -> Box<[u64]> {
        let guards: Vec<usize> = self
            .states
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t, Trans::DotGuard { .. }))
            .map(|(s, _)| s)
            .collect();
        if guards.is_empty() {
            return Box::new([]);
        }

        let stride = 1 + n_words;
        let mut recs = vec![0u64; guards.len() * stride];
        for (i, &g) in guards.iter().enumerate() {
            let Trans::DotGuard { next } = &self.states[g] else {
                unreachable!()
            };
            let rec = &mut recs[i * stride..(i + 1) * stride];
            rec[0] = g as u64;
            let base = *next as usize * n_words;
            rec[1..].copy_from_slice(&closures[base..base + n_words]);
        }

        let mut changed = true;
        while changed {
            changed = false;
            for i in (0..guards.len()).rev() {
                for (j, &gj) in guards.iter().enumerate() {
                    if i == j || recs[i * stride + 1 + (gj >> 6)] & (1u64 << (gj & 63)) == 0 {
                        continue;
                    }
                    for w in 0..n_words {
                        let merged = recs[i * stride + 1 + w] | recs[j * stride + 1 + w];
                        if merged != recs[i * stride + 1 + w] {
                            recs[i * stride + 1 + w] = merged;
                            changed = true;
                        }
                    }
                }
            }
        }
        recs.into_boxed_slice()
    }
}

struct Builder {
    states: Vec<Trans>,
    case_insensitive: bool,
    dot: bool,
}

impl Builder {
    fn new(case_insensitive: bool, dot: bool) -> Self {
        Self {
            states: Vec::with_capacity(32),
            case_insensitive,
            dot,
        }
    }

    fn alloc(&mut self, t: Trans) -> StateId {
        let id = self.next_id();
        self.states.push(t);
        id
    }

    fn next_id(&self) -> StateId {
        self.states.len() as StateId
    }

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
            // Every Split allocation site sets `a` eagerly; only `b` can
            // dangle (the zero-match exit of a dot=true star).
            Trans::Split { b, .. } => {
                if *b == UNSET {
                    *b = target;
                }
            }
        }
    }

    fn compile_ops(&mut self, ops: &[Op]) -> (StateId, Vec<StateId>) {
        if ops.is_empty() {
            let s = self.alloc(Trans::Jump { next: UNSET });
            return (s, vec![s]);
        }
        let (entry, mut pending_tails) = self.compile_op(&ops[0]);
        for op in &ops[1..] {
            let (op_entry, op_tails) = self.compile_op(op);
            for &tail in &pending_tails {
                self.patch(tail, op_entry);
            }
            pending_tails = op_tails;
        }
        (entry, pending_tails)
    }

    fn compile_op(&mut self, op: &Op) -> (StateId, Vec<StateId>) {
        match op {
            Op::Lit(bytes) => self.compile_lit(bytes),
            Op::AnyChar => self.compile_any_non_sep(),
            Op::Star => self.compile_star(),
            Op::Class(class) => self.compile_class(class),
            Op::Sep => self.compile_sep(),
            Op::SepRun => self.compile_sep_run(),
            Op::LeadingSeps => self.compile_leading_seps(),
            Op::OptSegmentsSlash => self.compile_oss(),
            Op::SlashAnything => self.compile_slash_anything(),
            Op::GlobstarAny => self.compile_globstar_any(),
            Op::Alternation(branches) => self.compile_alternation(branches),
            Op::Globstar => unreachable!("raw Globstar must be folded by lowering"),
        }
    }

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
            }),
            None => self.alloc(Trans::Byte { b, next: UNSET }),
        }
    }

    fn compile_any_non_sep(&mut self) -> (StateId, Vec<StateId>) {
        let s = self.alloc(Trans::AnyNonSep { next: UNSET });
        (s, vec![s])
    }

    fn compile_star(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.next_id();
        let body = entry + 1;
        if self.dot {
            self.alloc(Trans::Split { a: body, b: UNSET });
            self.alloc(Trans::AnyNonSep { next: entry });
            (entry, vec![entry])
        } else {
            let dot_guard = entry + 2;
            self.alloc(Trans::Split {
                a: body,
                b: dot_guard,
            });
            self.alloc(Trans::AnyNonSep { next: entry });
            self.alloc(Trans::DotGuard { next: UNSET });
            (entry, vec![dot_guard])
        }
    }

    fn compile_class(&mut self, class: &CharClass) -> (StateId, Vec<StateId>) {
        let s = self.alloc(Trans::Class {
            class: Box::new(class.clone()),
            next: UNSET,
        });
        (s, vec![s])
    }

    fn compile_sep(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Sep { next: UNSET });
        (entry, vec![entry])
    }

    fn compile_sep_run(&mut self) -> (StateId, Vec<StateId>) {
        let tail_split = self.next_id();
        let entry = tail_split + 1;
        self.alloc(Trans::Split { a: entry, b: UNSET });
        self.alloc(Trans::Sep { next: tail_split });
        (entry, vec![tail_split])
    }

    fn compile_leading_seps(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.next_id();
        let loop_body = entry + 1;
        self.alloc(Trans::Split {
            a: loop_body,
            b: UNSET,
        });
        self.alloc(Trans::Sep { next: entry });
        (entry, vec![entry])
    }

    fn compile_oss(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.next_id();
        let (seg_body, seg_cont, seg_body_loop, sep_start, sep_tail) =
            (entry + 1, entry + 2, entry + 3, entry + 4, entry + 5);
        self.alloc(Trans::Split {
            a: seg_body,
            b: UNSET,
        });
        self.alloc(Trans::AnyNonSep { next: seg_cont });
        self.alloc(Trans::Split {
            a: seg_body_loop,
            b: sep_start,
        });
        self.alloc(Trans::AnyNonSep { next: seg_cont });
        self.alloc(Trans::Sep { next: sep_tail });
        self.alloc(Trans::Split {
            a: sep_start,
            b: entry,
        });
        (entry, vec![entry])
    }

    fn compile_slash_anything(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.next_id();
        let (post_sep, tail, tail_loop) = (entry + 1, entry + 2, entry + 3);
        self.alloc(Trans::Sep { next: post_sep });
        self.alloc(Trans::Split { a: entry, b: tail });
        self.alloc(Trans::Split {
            a: tail_loop,
            b: UNSET,
        });
        self.alloc(Trans::AnyByte { next: tail });
        (entry, vec![tail])
    }

    fn compile_globstar_any(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.next_id();
        let body = entry + 1;
        self.alloc(Trans::Split { a: body, b: UNSET });
        self.alloc(Trans::AnyByte { next: entry });
        (entry, vec![entry])
    }

    fn compile_alternation(&mut self, branches: &[Vec<Op>]) -> (StateId, Vec<StateId>) {
        debug_assert!(branches.len() >= 2);
        let mut branch_entries = Vec::with_capacity(branches.len());
        let mut branch_tails = Vec::new();
        for branch in branches {
            let (entry, tails) = self.compile_ops(branch);
            branch_entries.push(entry);
            branch_tails.extend(tails);
        }
        let mut entry = *branch_entries.last().unwrap();
        for &a in branch_entries[..branch_entries.len() - 1].iter().rev() {
            entry = self.alloc(Trans::Split { a, b: entry });
        }
        (entry, branch_tails)
    }
}
