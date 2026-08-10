//! Pike VM interpreter over a compiled [`super::thompson::Thompson`] NFA.
//!
//! Linear-time O(n·m) matcher, the fallback when the segment engine can't
//! represent a pattern. Also kept as a standalone reference engine.
//!
//! # Algorithm
//!
//! Active states are a `u64` bitmap (`n_words = ceil(n / 64)`). For each byte
//! it walks the set bits with `trailing_zeros`, applies each leaf state's byte
//! test, and ORs the successor's pre-computed ε-closure into a `next` bitmap.
//! Static ε-closures (over Split and Jump, since DotGuard is byte-conditional)
//! are baked at build time, so per-step ε expansion is bitmap ORs instead of a
//! recursive walk.
//!
//! # Dot guards
//!
//! `dot=false` compiles emit byte-conditional [`Trans::DotGuard`] ε-states
//! that static closures cannot absorb (whether a guard passes depends on the
//! upcoming byte). Each guard's transitive pass-expansion is precomputed at
//! build time; when the guard condition holds, the run loop ORs the tables of
//! the active guards in one pass before the sweep. A failing guard's thread
//! simply dies, since `byte_step` ignores guard states.
//!
//! # Scratch on the stack
//!
//! Scratch is one contiguous `[u64; STACK_WORDS * N]` array on the call stack,
//! with no heap and no `Sync` concerns. An NFA larger than `STACK_WORDS * 64`
//! states falls back to a per-call `Vec<u64>`.

use crate::dir_match::DirMatch;
use crate::engine::facts::LiteralFacts;
use crate::engine::ops::{OpProgram, compute_static_prefixes};
use crate::engine::thompson::{StateId, Thompson, Trans};

/// Stack-allocated bitmap word budget: `4` words cover NFAs up to 256
/// states; larger ones heap-allocate.
const STACK_WORDS: usize = 4;

/// Scratch slots for `is_match` (current, next).
const RUN_SLOTS: usize = 2;
/// Scratch slots for `match_dir`, `RUN_SLOTS` plus `after_sep` for the
/// prefix-descent probe.
const DIR_SLOTS: usize = 3;

/// Pike VM matcher, compiled once per pattern and `Send + Sync` (no interior
/// mutability, scratch is per-call on the stack).
///
/// Stores only what the byte-step needs. The full [`Thompson`] is consulted
/// during construction and then dropped.
#[derive(Debug, Clone)]
pub struct PikeVm {
    /// Trans table.
    states: Box<[Trans]>,
    facts: LiteralFacts,
    prefixes: Box<[Box<[u8]>]>,
    /// Bitmap of states from which a non-empty byte sequence can reach
    /// [`Trans::Match`] (bit `s` set iff state `s` qualifies). Drives the
    /// prefix-mode descent test in [`PikeVm::match_dir_inner`].
    reach_to_accept: Box<[u64]>,
    /// `ceil(states.len() / 64)`. Length of every bitmap below.
    n_words: usize,
    /// Per-NFA-state ε-closure (Split/Jump only) packed as `n × n_words`
    /// `u64` bitmaps. `static_closures[s * n_words .. (s+1) * n_words]`
    /// is the bitmap of leaves reachable from `s`.
    static_closures: Box<[u64]>,
    /// ε-closure of the initial state, copied into `current` at the start of
    /// each match.
    init_bits: Box<[u64]>,
    /// `accepts_at_eof` packed as a bitmap so the EOF accept check is
    /// one AND across `n_words` words.
    accept_bits: Box<[u64]>,
    /// Packed DotGuard pass-expansions, one `1 + n_words` record per guard
    /// (state id, then the transitive bitmap it releases when it passes).
    /// Empty for `dot=true` compiles. See [`Thompson::guard_expansions`].
    guard_exps: Box<[u64]>,
}

impl PikeVm {
    /// Compile the program into a Pike VM. Folds the Thompson fields the
    /// runtime needs into bitmaps, then drops the rest.
    pub fn new(program: OpProgram, dot: bool) -> Self {
        let thompson = Thompson::compile(&program, dot);
        let reach_flags = thompson.reach_to_accept();
        let prefixes = compute_static_prefixes(&program.ops);
        let facts = program.facts;

        let n = thompson.states.len();
        let n_words = n.div_ceil(64);
        let static_closures = thompson.static_closures(n_words).into_boxed_slice();

        let init_off = (thompson.initial as usize) * n_words;
        let init_bits = static_closures[init_off..init_off + n_words]
            .to_vec()
            .into_boxed_slice();

        let mut accept_bits = vec![0u64; n_words];
        for (i, &eof) in thompson.accepts_at_eof.iter().enumerate() {
            if eof {
                accept_bits[i >> 6] |= 1u64 << (i & 63);
            }
        }

        let mut reach_to_accept = vec![0u64; n_words].into_boxed_slice();
        for (i, &reach) in reach_flags.iter().enumerate() {
            if reach {
                reach_to_accept[i >> 6] |= 1u64 << (i & 63);
            }
        }

        let guard_exps = thompson.guard_expansions(&static_closures, n_words);

        // Keep only `states`. `initial`, `accept`, and `accepts_at_eof` now
        // live in `init_bits`, `reach_to_accept`, and `accept_bits`.
        let Thompson { states, .. } = thompson;

        Self {
            states: states.into_boxed_slice(),
            facts,
            prefixes,
            reach_to_accept,
            n_words,
            static_closures,
            init_bits,
            accept_bits: accept_bits.into_boxed_slice(),
            guard_exps,
        }
    }

    /// Cached static path prefixes for walker integration.
    pub fn static_prefixes(&self) -> &[Box<[u8]>] {
        &self.prefixes
    }

    /// Full-match query: the whole `path` must match the pattern.
    pub fn is_match(&self, path: &[u8]) -> bool {
        if !self.facts.accept(path) {
            return false;
        }
        let nw = self.n_words;
        if nw <= STACK_WORDS {
            let mut buf = [0u64; STACK_WORDS * RUN_SLOTS];
            self.is_match_inner(path, &mut buf[..nw * RUN_SLOTS])
        } else {
            self.is_match_inner(path, &mut vec![0u64; nw * RUN_SLOTS])
        }
    }

    /// Walker-style directory query: does `dir_path` match exactly,
    /// descend into a possible match, or both?
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        // Empty dir path is the cwd and every match lives under it, so
        // descent is always on. The probe below would instead simulate a
        // leading `/`, which cwd children don't have.
        if dir_path.is_empty() {
            return DirMatch::from_exact_prefix(self.is_match(&[]), true);
        }
        let nw = self.n_words;
        if nw <= STACK_WORDS {
            let mut buf = [0u64; STACK_WORDS * DIR_SLOTS];
            self.match_dir_inner(dir_path, &mut buf[..nw * DIR_SLOTS])
        } else {
            self.match_dir_inner(dir_path, &mut vec![0u64; nw * DIR_SLOTS])
        }
    }

    // ── Inner implementations (shared between stack and heap paths) ──

    fn is_match_inner(&self, path: &[u8], buf: &mut [u64]) -> bool {
        let nw = self.n_words;
        buf[..nw].copy_from_slice(&self.init_bits);
        self.run(path, buf, nw);
        bitmap_intersects(&buf[..nw], &self.accept_bits)
    }

    fn match_dir_inner(&self, dir_path: &[u8], buf: &mut [u64]) -> DirMatch {
        let nw = self.n_words;
        buf[..nw].copy_from_slice(&self.init_bits);
        // Split off the trailing `after_sep` slot before running so the
        // run loop sees only `[current, next]`.
        let (active, after_sep) = buf.split_at_mut(nw * RUN_SLOTS);
        self.run(dir_path, active, nw);
        let exact = bitmap_intersects(&active[..nw], &self.accept_bits);

        // Probe a hypothetical descendant `/` step, then a reach-to-accept
        // check. That `/` follows a non-separator byte (the dir's last byte),
        // so `at_seg_start` is false.
        let after_sep = &mut after_sep[..nw];
        after_sep.fill(0);
        let states = &self.states;
        let closures = &self.static_closures;

        // ε-expand guards before the hypothetical `/`. A separator is never a
        // segment-start dot, so every guard passes here. Static closures stop
        // at a guard, so a Sep or AnyByte consumer behind one (the trailing
        // `/` of `*/` under dot=false) would otherwise be invisible and the
        // subtree wrongly pruned.
        self.expand_guards(&mut active[..nw]);

        for s in iter_set_states(&active[..nw]) {
            if let Some(n) = byte_step(&states[s], b'/', true, false) {
                let base = (n as usize) * nw;
                for j in 0..nw {
                    after_sep[j] |= closures[base + j];
                }
            }
        }
        // Prefix-mode descent qualifier. A state in `after_sep` either is
        // Match or can reach Match with more bytes.
        let reach_hit = bitmap_intersects(after_sep, &self.reach_to_accept);
        let match_hit =
            !reach_hit && iter_set_states(after_sep).any(|s| matches!(states[s], Trans::Match));
        let prefix = reach_hit || match_hit;

        DirMatch::from_exact_prefix(exact, prefix)
    }

    // ── Per-byte run loop ───────────────────────────────────────────

    /// One sweep per byte. When the guard condition holds, active guards
    /// expand through their precomputed tables before the sweep; a failing
    /// guard's thread dies since `byte_step` ignores guard states. `current`
    /// and `next` flip by swapping offsets, and the final active set is
    /// copied back to slot 0.
    ///
    /// Monomorphized on guard presence, so the guard-free (`dot=true`) loop
    /// carries no per-byte check at all.
    fn run(&self, path: &[u8], buf: &mut [u64], nw: usize) {
        if self.guard_exps.is_empty() {
            self.run_inner::<false>(path, buf, nw)
        } else {
            self.run_inner::<true>(path, buf, nw)
        }
    }

    fn run_inner<const GUARDS: bool>(&self, path: &[u8], buf: &mut [u64], nw: usize) {
        // Hoist field reads out of the hot loop.
        let states = &self.states;
        let closures = &self.static_closures;
        let mut cur = 0usize;
        let mut nxt = nw;
        let mut at_seg_start = true;

        for &c in path {
            buf[nxt..nxt + nw].fill(0);
            let sep = std::path::is_separator(c as char);
            let dot_mask = at_seg_start && c == b'.';

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

    /// OR each active guard's precomputed pass-expansion into `cur`. The
    /// records are transitive, so one pass covers guard chains. No-op when
    /// the NFA has no guard.
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

// ── Bitmap helpers ──────────────────────────────────────────────────

/// `(a & b) ≠ 0` over two equal-length bitmaps.
#[inline]
fn bitmap_intersects(a: &[u64], b: &[u64]) -> bool {
    a.iter().zip(b.iter()).any(|(x, y)| (x & y) != 0)
}

/// Iterate the set bits of `bits` as flat NFA-state indices. Each step clears
/// the lowest set bit (`word &= word - 1`), so the work scales with the
/// popcount, not 64.
fn iter_set_states(bits: &[u64]) -> impl Iterator<Item = usize> + '_ {
    bits.iter().enumerate().flat_map(|(w_idx, &word)| {
        let base = w_idx * 64;
        std::iter::from_fn({
            let mut word = word;
            move || {
                if word == 0 {
                    return None;
                }
                let bit = word.trailing_zeros() as usize;
                word &= word - 1;
                Some(base + bit)
            }
        })
    })
}

/// Apply one leaf state's byte test against `c`. Returns the successor state
/// if the transition fires, else `None`. The caller expands the ε-closure.
///
/// `DotGuard` returns `None` because it consumes nothing: a passing guard is
/// expanded through its precomputed table before the sweep, and a failing one
/// dies right here. Match, Split, and Jump never appear in the active set
/// after ε-closure, and Match consumes no byte.
#[inline]
fn byte_step(t: &Trans, c: u8, sep: bool, dot_mask: bool) -> Option<StateId> {
    match t {
        Trans::Byte { b, next: n } => (*b == c).then_some(*n),
        Trans::Class {
            class,
            next: n,
            dot_protected,
        } => (class.matches(c) && !(*dot_protected && dot_mask)).then_some(*n),
        Trans::AnyNonSep {
            next: n,
            dot_protected,
        } => (!(sep || *dot_protected && dot_mask)).then_some(*n),
        Trans::AnyByte {
            next: n,
            dot_protected,
        } => (!(*dot_protected && dot_mask)).then_some(*n),
        Trans::Sep { next: n } => sep.then_some(*n),
        Trans::DotGuard { .. } | Trans::Match | Trans::Split { .. } | Trans::Jump { .. } => None,
    }
}
