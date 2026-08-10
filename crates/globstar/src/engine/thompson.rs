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

        // Fold guard chains until stable. Chains point forward (a guard's
        // expansion only ever holds later guards), so the reverse pass folds
        // each chain in one go and the loop converges in ~2 passes. Order
        // affects speed only, the fixpoint is unique.
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

    fn compile_ops(&mut self, ops: &[Op]) -> (StateId, Vec<StateId>) {
        if ops.is_empty() {
            let s = self.alloc(Trans::Jump { next: UNSET });
            return (s, vec![s]);
        }
        let mut entry: Option<StateId> = None;
        let mut pending_tails: Vec<StateId> = Vec::new();
        for op in ops {
            let (op_entry, mut op_tails) = self.compile_op(op);
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
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → body
            b: UNSET, // → dot_guard or exit
        });
        let body = self.alloc(Trans::AnyNonSep { next: entry });
        if let Trans::Split { a, .. } = &mut self.states[entry as usize] {
            *a = body;
        }
        if !self.dot {
            let dot_guard = self.alloc(Trans::DotGuard { next: UNSET });
            if let Trans::Split { b, .. } = &mut self.states[entry as usize] {
                *b = dot_guard;
            }
            (entry, vec![dot_guard])
        } else {
            (entry, vec![entry])
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

    fn compile_oss(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → seg_body
            b: UNSET, // → exit
        });
        let seg_body = self.alloc(Trans::AnyNonSep { next: UNSET });
        let seg_cont = self.alloc(Trans::Split {
            a: UNSET, // → seg_body_loop (more non-sep bytes)
            b: UNSET, // → sep_start (end of segment)
        });
        let seg_body_loop = self.alloc(Trans::AnyNonSep { next: seg_cont });
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

    fn compile_slash_anything(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Sep { next: UNSET });
        let post_sep = self.alloc(Trans::Split {
            a: UNSET, // → entry (collapse consecutive seps)
            b: UNSET, // → tail
        });
        let tail = self.alloc(Trans::Split {
            a: UNSET, // → tail_loop (more bytes)
            b: UNSET, // → exit
        });
        let tail_loop = self.alloc(Trans::AnyByte { next: tail });

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

    fn compile_globstar_any(&mut self) -> (StateId, Vec<StateId>) {
        let entry = self.alloc(Trans::Split {
            a: UNSET, // → body
            b: UNSET, // → exit
        });
        let body = self.alloc(Trans::AnyByte { next: entry });
        if let Trans::Split { a, .. } = &mut self.states[entry as usize] {
            *a = body;
        }
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
