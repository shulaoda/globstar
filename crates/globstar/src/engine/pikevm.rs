use crate::dir_match::DirMatch;
use crate::engine::facts::LiteralFacts;
use crate::engine::ops::{OpProgram, compute_static_prefixes};
use crate::engine::thompson::{StateId, Thompson, Trans};

const RUN_SLOTS: usize = 2;
const STACK_WORDS: usize = 4;

#[derive(Debug, Clone)]
pub struct PikeVm {
    states: Box<[Trans]>,
    facts: LiteralFacts,
    prefixes: Box<[Box<[u8]>]>,
    /// States that accept as-is or can reach [`Trans::Match`] with more
    /// bytes. Drives the descent half of `match_dir`.
    descend_bits: Box<[u64]>,
    /// `ceil(states.len() / 64)`. Length of every bitmap below.
    n_words: usize,
    /// Per-state ε-closure (Split/Jump only), `n × n_words` bitmaps.
    static_closures: Box<[u64]>,
    /// Offset of the initial state's closure row in `static_closures`.
    init_off: usize,
    /// States that accept at end of input. The Match state, plus every guard
    /// whose expansion reaches it (a guard passes unconditionally at EOF).
    accept_bits: Box<[u64]>,
    /// Packed DotGuard expansions, see [`Thompson::guard_expansions`].
    guard_exps: Box<[u64]>,
    /// `!dot`: wildcards refuse a segment-start `.` (GLOB_SPEC §6).
    dot_protect: bool,
}

impl PikeVm {
    pub fn new(program: OpProgram, dot: bool) -> Self {
        let thompson = Thompson::compile(&program, dot);
        let prefixes = compute_static_prefixes(&program.ops);
        let facts = program.facts;

        let n = thompson.states.len();
        let n_words = n.div_ceil(64);
        let static_closures = thompson.static_closures(n_words).into_boxed_slice();
        let guard_exps = thompson.guard_expansions(&static_closures, n_words);
        let init_off = (thompson.initial as usize) * n_words;

        let accept = thompson.accept as usize;
        let mut accept_bits = vec![0u64; n_words].into_boxed_slice();
        accept_bits[accept >> 6] |= 1u64 << (accept & 63);
        for rec in guard_exps.chunks_exact(1 + n_words) {
            if rec[1 + (accept >> 6)] & (1u64 << (accept & 63)) != 0 {
                let g = rec[0] as usize;
                accept_bits[g >> 6] |= 1u64 << (g & 63);
            }
        }

        // Fixpoint seeded with `accept_bits`. Leaf `s` joins iff
        // closure(next(s)) hits the set. Reverse order follows the backward
        // flow (Thompson allocates successors after their predecessors), so
        // this converges in ~2 passes. Order affects speed only, the least
        // fixpoint is unique.
        let mut descend_bits = accept_bits.clone();
        let mut changed = true;
        while changed {
            changed = false;
            for (s, t) in thompson.states.iter().enumerate().rev() {
                if descend_bits[s >> 6] & (1u64 << (s & 63)) != 0 {
                    continue;
                }
                let next = match t {
                    Trans::Byte { next, .. }
                    | Trans::Class { next, .. }
                    | Trans::AnyNonSep { next }
                    | Trans::AnyByte { next }
                    | Trans::Sep { next }
                    | Trans::DotGuard { next } => *next as usize,
                    Trans::Match | Trans::Split { .. } | Trans::Jump { .. } => continue,
                };
                let base = next * n_words;
                let hit = (0..n_words).any(|w| static_closures[base + w] & descend_bits[w] != 0);
                if hit {
                    descend_bits[s >> 6] |= 1u64 << (s & 63);
                    changed = true;
                }
            }
        }

        let states = thompson.states.into_boxed_slice();

        Self {
            states,
            facts,
            prefixes,
            descend_bits,
            n_words,
            static_closures,
            init_off,
            accept_bits,
            guard_exps,
            dot_protect: !dot,
        }
    }

    pub fn static_prefixes(&self) -> &[Box<[u8]>] {
        &self.prefixes
    }

    pub fn is_match(&self, path: &[u8]) -> bool {
        if !self.facts.accept(path) {
            return false;
        }
        with_scratch(self.n_words, |buf| {
            let nw = self.n_words;
            buf[..nw].copy_from_slice(&self.static_closures[self.init_off..self.init_off + nw]);
            self.run(path, buf, nw);
            bitmap_intersects(&buf[..nw], &self.accept_bits)
        })
    }

    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        if dir_path.is_empty() {
            return DirMatch::from_exact_prefix(self.is_match(&[]), true);
        }
        with_scratch(self.n_words, |buf| {
            let nw = self.n_words;
            buf[..nw].copy_from_slice(&self.static_closures[self.init_off..self.init_off + nw]);
            self.run(dir_path, buf, nw);

            let (active, after_sep) = buf.split_at_mut(nw);
            let exact = bitmap_intersects(active, &self.accept_bits);

            self.expand_guards(active);

            after_sep.fill(0);
            let states = &self.states;
            let closures = &self.static_closures;
            for (w_idx, &active_word) in active.iter().enumerate() {
                let mut word = active_word;
                while word != 0 {
                    let s = w_idx * 64 + word.trailing_zeros() as usize;
                    word &= word - 1;
                    if let Some(n) = byte_step(&states[s], b'/', true, false) {
                        let base = (n as usize) * nw;
                        for j in 0..nw {
                            after_sep[j] |= closures[base + j];
                        }
                    }
                }
            }
            DirMatch::from_exact_prefix(exact, bitmap_intersects(after_sep, &self.descend_bits))
        })
    }

    fn run(&self, path: &[u8], buf: &mut [u64], nw: usize) {
        let states = &self.states;
        let closures = &self.static_closures;
        let protect = self.dot_protect;
        let has_guards = !self.guard_exps.is_empty();
        let mut cur = 0usize;
        let mut nxt = nw;
        let mut at_seg_start = true;

        for &c in path {
            buf[nxt..nxt + nw].fill(0);
            let sep = std::path::is_separator(c as char);
            let dot_mask = protect && at_seg_start && c == b'.';

            if has_guards && !dot_mask {
                self.expand_guards(&mut buf[cur..cur + nw]);
            }

            for w_idx in 0..nw {
                let mut word = buf[cur + w_idx];
                while word != 0 {
                    let s = w_idx * 64 + word.trailing_zeros() as usize;
                    word &= word - 1;
                    if let Some(n) = byte_step(&states[s], c, sep, dot_mask) {
                        let base = (n as usize) * nw;
                        for j in 0..nw {
                            buf[nxt + j] |= closures[base + j];
                        }
                    }
                }
            }

            std::mem::swap(&mut cur, &mut nxt);
            at_seg_start = sep;
            if buf[cur..cur + nw].iter().all(|&w| w == 0) {
                break;
            }
        }

        if cur != 0 {
            buf.copy_within(cur..cur + nw, 0);
        }
    }

    #[inline]
    fn expand_guards(&self, cur: &mut [u64]) {
        let nw = self.n_words;
        for rec in self.guard_exps.chunks_exact(1 + nw) {
            let g = rec[0] as usize;
            if cur[g >> 6] & (1u64 << (g & 63)) != 0 {
                for (w, exp) in cur.iter_mut().zip(&rec[1..]) {
                    *w |= exp;
                }
            }
        }
    }
}

fn with_scratch<R>(nw: usize, f: impl FnOnce(&mut [u64]) -> R) -> R {
    if nw <= STACK_WORDS {
        f(&mut [0u64; STACK_WORDS * RUN_SLOTS][..nw * RUN_SLOTS])
    } else {
        f(&mut vec![0u64; nw * RUN_SLOTS])
    }
}

#[inline]
fn bitmap_intersects(a: &[u64], b: &[u64]) -> bool {
    a.iter().zip(b.iter()).any(|(x, y)| (x & y) != 0)
}

fn byte_step(t: &Trans, c: u8, sep: bool, dot_mask: bool) -> Option<StateId> {
    match t {
        Trans::Byte { b, next: n } => (*b == c).then_some(*n),
        Trans::Class { class, next: n } => {
            (class.matches(c) && !(class.negated && dot_mask)).then_some(*n)
        }
        Trans::AnyNonSep { next: n } => (!(sep || dot_mask)).then_some(*n),
        Trans::AnyByte { next: n } => (!dot_mask).then_some(*n),
        Trans::Sep { next: n } => sep.then_some(*n),
        Trans::DotGuard { .. } | Trans::Match | Trans::Split { .. } | Trans::Jump { .. } => None,
    }
}
