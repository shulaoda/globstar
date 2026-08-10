use crate::dir_match::DirMatch;
use crate::engine::facts::LiteralFacts;
use crate::engine::ops::{OpProgram, compute_static_prefixes};
use crate::engine::thompson::{StateId, Thompson, Trans};

/// Bitmap words on the call stack. `4` words cover NFAs up to 256 states,
/// larger ones heap-allocate per call.
const STACK_WORDS: usize = 4;

/// Scratch slots (current, next). After `run` the final set sits in slot 0,
/// so `match_dir` reuses the dead slot 1 for its `/` probe.
const RUN_SLOTS: usize = 2;

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
                let hit =
                    (0..n_words).any(|w| static_closures[base + w] & descend_bits[w] != 0);
                if hit {
                    descend_bits[s >> 6] |= 1u64 << (s & 63);
                    changed = true;
                }
            }
        }

        let Thompson { states, .. } = thompson;

        Self {
            states: states.into_boxed_slice(),
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

    /// Static path prefixes for walker integration.
    pub fn static_prefixes(&self) -> &[Box<[u8]>] {
        &self.prefixes
    }

    pub fn is_match(&self, path: &[u8]) -> bool {
        if !self.facts.accept(path) {
            return false;
        }
        with_scratch(self.n_words, |buf| self.is_match_inner(path, buf))
    }

    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        // The empty dir is the cwd and every match lives under it, so descent
        // is always on. The probe below would instead simulate a leading `/`,
        // which cwd children don't have.
        if dir_path.is_empty() {
            return DirMatch::from_exact_prefix(self.is_match(&[]), true);
        }
        with_scratch(self.n_words, |buf| self.match_dir_inner(dir_path, buf))
    }

    fn is_match_inner(&self, path: &[u8], buf: &mut [u64]) -> bool {
        let nw = self.n_words;
        buf[..nw].copy_from_slice(&self.static_closures[self.init_off..self.init_off + nw]);
        self.run(path, buf, nw);
        bitmap_intersects(&buf[..nw], &self.accept_bits)
    }

    fn match_dir_inner(&self, dir_path: &[u8], buf: &mut [u64]) -> DirMatch {
        let nw = self.n_words;
        buf[..nw].copy_from_slice(&self.static_closures[self.init_off..self.init_off + nw]);
        self.run(dir_path, buf, nw);
        // `run` left the final active set in slot 0, slot 1 is dead and
        // becomes the probe's `after_sep`.
        let (active, after_sep) = buf.split_at_mut(nw);
        let exact = bitmap_intersects(active, &self.accept_bits);

        // Expand guards first. A `/` is never a segment-start dot, so every
        // guard passes here, and a consumer hiding behind one (the trailing
        // `/` of `*/` under dot=false) would otherwise be invisible and the
        // subtree wrongly pruned.
        self.expand_guards(active);

        // Probe one hypothetical descendant `/` step. It follows the dir's
        // last byte, which is not a separator, so `at_seg_start` is false.
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
        // Descend iff some surviving state accepts as-is or can still reach
        // Match with more bytes.
        let prefix = bitmap_intersects(after_sep, &self.descend_bits);

        DirMatch::from_exact_prefix(exact, prefix)
    }

    /// One bitmap sweep per byte. Monomorphized on guard presence so the
    /// guard-free (`dot=true`) loop carries no per-byte check at all.
    fn run(&self, path: &[u8], buf: &mut [u64], nw: usize) {
        if self.guard_exps.is_empty() {
            self.run_inner::<false>(path, buf, nw)
        } else {
            self.run_inner::<true>(path, buf, nw)
        }
    }

    fn run_inner<const GUARDS: bool>(&self, path: &[u8], buf: &mut [u64], nw: usize) {
        let states = &self.states;
        let closures = &self.static_closures;
        let protect = self.dot_protect;
        let mut cur = 0usize;
        let mut nxt = nw;
        let mut at_seg_start = true;

        for &c in path {
            buf[nxt..nxt + nw].fill(0);
            let sep = std::path::is_separator(c as char);
            let dot_mask = protect && at_seg_start && c == b'.';

            if GUARDS && !dot_mask {
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

        // Caller expects the final active set in `buf[..nw]`.
        if cur != 0 {
            buf.copy_within(cur..cur + nw, 0);
        }
    }

    /// OR each active guard's pass-expansion into `cur`. The records are
    /// transitive, so one pass covers guard chains.
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

/// Run `f` on a zeroed scratch of `nw * RUN_SLOTS` words, on the stack when
/// the NFA is small enough.
fn with_scratch<R>(nw: usize, f: impl FnOnce(&mut [u64]) -> R) -> R {
    if nw <= STACK_WORDS {
        f(&mut [0u64; STACK_WORDS * RUN_SLOTS][..nw * RUN_SLOTS])
    } else {
        f(&mut vec![0u64; nw * RUN_SLOTS])
    }
}

/// `(a & b) ≠ 0` over two equal-length bitmaps.
#[inline]
fn bitmap_intersects(a: &[u64], b: &[u64]) -> bool {
    a.iter().zip(b.iter()).any(|(x, y)| (x & y) != 0)
}

/// One leaf state's byte test. Returns the successor if the transition
/// fires, the caller expands its ε-closure.
///
/// `dot_mask` is true only for a segment-start `.` under dot protection.
/// Wildcards and negated classes refuse it, literals and positive classes
/// match it. A passing `DotGuard` is expanded before the sweep, so `None`
/// here kills only failing guards.
#[inline]
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
