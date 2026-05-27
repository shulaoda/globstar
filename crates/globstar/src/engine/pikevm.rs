//! Pike VM interpreter over a compiled [`super::thompson::Thompson`] NFA.
//!
//! Linear-time O(n·m) matcher, used as the Tier-1/2 fallback when
//! [`super::thompson_dfa::ThompsonDfa`] subset construction overruns
//! its state cap, and as a standalone engine when callers want a
//! fast-to-compile matcher.
//!
//! # Algorithm
//!
//! Active NFA states are tracked as a `u64` bitmap (`n_words = ceil(n
//! / 64)`). Per byte: iterate set bits via `trailing_zeros`, apply
//! the byte test of each leaf state, OR the successor's pre-computed
//! ε-closure bitmap into a `next` bitmap. Static ε-closures (over
//! Split/Jump only — DotGuard is byte-conditional) are baked at
//! build time, so per-step ε expansion is `O(active × n_words)`
//! bitmap ORs rather than a recursive BFS.
//!
//! # Two run paths
//!
//! - [`PikeVm::run_fast`] — single sweep per byte. Used when the NFA
//!   has no [`Trans::DotGuard`] (default `dot=true` compile).
//! - [`PikeVm::run_dot_guard`] — work-queue with `processed` dedup
//!   for the byte-conditional ε of DotGuard. Used when `dot=false`
//!   produces DotGuard states.
//!
//! # Scratch on the stack
//!
//! Scratch (`current` / `next` / optional `processed` / optional
//! `after_sep`) is allocated as ONE contiguous `[u64; STACK_WORDS *
//! N]` array on the call stack. No `Mutex`, no heap allocation, no
//! `Sync` concerns. Patterns whose NFA exceeds `STACK_WORDS * 64`
//! states fall back to a single per-call `Vec<u64>` allocation —
//! rare path.

use crate::dir_match::DirMatch;
use crate::engine::facts::LiteralFacts;
use crate::engine::ops::{OpProgram, compute_static_prefixes};
use crate::engine::thompson::{
    StateId, Thompson, Trans, compute_reach_to_accept, compute_static_closures,
};

/// Stack-allocated bitmap word budget. `STACK_WORDS = 4` covers NFAs
/// up to 256 states — fits every pattern in our corpus, including
/// the 223-state `huge-set-pos` union. Larger NFAs heap-allocate.
const STACK_WORDS: usize = 4;

/// Slot count for `is_match` scratch: `current`, `next`, `processed`.
const RUN_SLOTS: usize = 3;
/// Slot count for `match_dir` scratch: same as `RUN_SLOTS` plus an
/// `after_sep` slot for the prefix-descent probe.
const DIR_SLOTS: usize = 4;

/// Pike VM matcher. Compiled once per pattern; `Send + Sync` (no
/// interior mutability — scratch is per-call on the stack).
///
/// Stores only what the runtime byte-step actually needs. The full
/// [`Thompson`] structure is consulted during construction and then
/// dropped: `Thompson::initial`, `accept`, and `accepts_at_eof` get
/// folded into `init_bits` / `accept_bits` / `reach_to_accept` so
/// their backing storage can be reclaimed.
#[derive(Debug, Clone)]
pub struct PikeVm {
    /// Trans table. `Box<[Trans]>` rather than `Vec<Trans>` — saves
    /// 8 B inline (no `cap` field) and signals the fixed-size nature.
    states: Box<[Trans]>,
    facts: LiteralFacts,
    prefixes: Box<[Box<[u8]>]>,
    /// Bitmap of states from which a non-empty byte sequence can
    /// reach [`Trans::Match`]. Length `n_words`, bit `s` set iff
    /// state `s` qualifies. Drives the prefix-mode descent test in
    /// [`PikeVm::match_dir_inner`]. Packed (vs `Box<[bool]>`) so the
    /// per-state flag costs 1 bit instead of 1 byte — saves
    /// `~N - 8·n_words` bytes per matcher.
    reach_to_accept: Box<[u64]>,
    /// `ceil(states.len() / 64)`. Length of every bitmap below.
    n_words: usize,
    /// Per-NFA-state ε-closure (Split/Jump only) packed as `n × n_words`
    /// `u64` bitmaps. `static_closures[s * n_words .. (s+1) * n_words]`
    /// is the bitmap of leaves reachable from `s`.
    static_closures: Box<[u64]>,
    /// ε-closure of the initial NFA state — copied into `current` at
    /// the start of every match.
    init_bits: Box<[u64]>,
    /// `accepts_at_eof` packed as a bitmap so the EOF accept check is
    /// one AND across `n_words` words.
    accept_bits: Box<[u64]>,
    /// Drives dispatch between [`Self::run_fast`] (no DotGuard) and
    /// [`Self::run_dot_guard`].
    has_dot_guard: bool,
}

impl PikeVm {
    /// Compile the program into a Pike VM. Folds every Thompson field
    /// the runtime needs into compact bitmaps / boxed slices and then
    /// drops the rest — no NFA metadata lingers past construction.
    pub fn new(program: OpProgram, dot: bool) -> Self {
        let thompson = Thompson::compile(&program, dot);
        let reach_flags = compute_reach_to_accept(&thompson.states, thompson.accept);
        let prefixes = compute_static_prefixes(&program.ops);
        let facts = program.facts;

        let n = thompson.states.len();
        let n_words = n.div_ceil(64);
        let static_closures = compute_static_closures(&thompson, n_words).into_boxed_slice();

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

        let has_dot_guard = thompson
            .states
            .iter()
            .any(|t| matches!(t, Trans::DotGuard { .. }));

        // Destructure to keep only `states`; `initial`, `accept`, and
        // `accepts_at_eof` are now redundant — their content lives in
        // `init_bits`, `reach_to_accept`, and `accept_bits` above.
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
            has_dot_guard,
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
        if self.has_dot_guard {
            self.run_dot_guard(path, buf, nw);
        } else {
            self.run_fast(path, buf, nw);
        }
        bitmap_intersects(&buf[..nw], &self.accept_bits)
    }

    fn match_dir_inner(&self, dir_path: &[u8], buf: &mut [u64]) -> DirMatch {
        let nw = self.n_words;
        buf[..nw].copy_from_slice(&self.init_bits);
        // Split off the trailing `after_sep` slot before running so the
        // run loop sees only `[current, next, processed]`.
        let (active, after_sep) = buf.split_at_mut(nw * RUN_SLOTS);
        if self.has_dot_guard {
            self.run_dot_guard(dir_path, active, nw);
        } else {
            self.run_fast(dir_path, active, nw);
        }
        let exact = bitmap_intersects(&active[..nw], &self.accept_bits);

        // Hypothetical descendant '/' step + reach-to-accept check.
        // The descendant '/' is consumed after a non-separator byte
        // (the dir's trailing byte), so `at_seg_start = false`.
        let after_sep = &mut after_sep[..nw];
        after_sep.fill(0);
        let states = &self.states;
        let closures = &self.static_closures;
        for s in iter_set_states(&active[..nw]) {
            if let Some(n) = byte_step(&states[s], b'/', true, false) {
                let base = (n as usize) * nw;
                for j in 0..nw {
                    after_sep[j] |= closures[base + j];
                }
            }
        }
        // Prefix-mode descent qualifier: some state in `after_sep` is
        // either Match itself or can reach Match via more bytes. The
        // reach-to-accept check is a single bitmap AND; the Match
        // check is a tiny loop over the (typically 1-3) set bits.
        let reach_hit = bitmap_intersects(after_sep, &self.reach_to_accept);
        let match_hit =
            !reach_hit && iter_set_states(after_sep).any(|s| matches!(states[s], Trans::Match));
        let prefix = reach_hit || match_hit;

        DirMatch::from_exact_prefix(exact, prefix)
    }

    // ── Per-byte run loops ──────────────────────────────────────────

    /// Fast path (no DotGuard): one sweep per byte. The two slots in
    /// `buf` (current and next) flip via `mem::swap` of the offsets;
    /// the final active set is copied back to slot 0 before return.
    fn run_fast(&self, path: &[u8], buf: &mut [u64], nw: usize) {
        // Hoist field reads outside the hot loop so the inner code
        // compiles to direct slice access without re-resolving `self`
        // per iteration. Measurable on short paths where per-byte
        // overhead dominates.
        let states = &self.states;
        let closures = &self.static_closures;
        let mut cur = 0usize;
        let mut nxt = nw;
        let mut at_seg_start = true;

        for &c in path {
            buf[nxt..nxt + nw].fill(0);
            let sep = std::path::is_separator(c as char);
            let dot_mask = at_seg_start && c == b'.';

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

    /// DotGuard path: byte-conditional ε can re-fill `current` mid-
    /// step. We drain it via a work-queue with `processed` dedup
    /// before swapping into `next`. Slots are fixed at `[current,
    /// next, processed]` since `current` mutates in place.
    fn run_dot_guard(&self, path: &[u8], buf: &mut [u64], nw: usize) {
        let states = &self.states;
        let closures = &self.static_closures;
        let cur = 0;
        let nxt = nw;
        let proc = nw * 2;
        let mut at_seg_start = true;

        for &c in path {
            buf[nxt..proc + nw].fill(0); // zero next AND processed
            let sep = std::path::is_separator(c as char);
            let dot_mask = at_seg_start && c == b'.';

            // Drain `current` until no new bits are added.
            loop {
                let mut found_new_work = false;
                for w_idx in 0..nw {
                    let unprocessed = buf[cur + w_idx] & !buf[proc + w_idx];
                    if unprocessed == 0 {
                        continue;
                    }
                    found_new_work = true;
                    buf[proc + w_idx] |= unprocessed;

                    let mut word = unprocessed;
                    while word != 0 {
                        let s = w_idx * 64 + word.trailing_zeros() as usize;
                        word &= word - 1;
                        // Byte-consumers fire to `next`; DotGuard
                        // ε-additions land in `current` to be re-
                        // processed by the outer drain loop.
                        let (target, n) = match &states[s] {
                            Trans::DotGuard { next: n } if !dot_mask => (cur, *n),
                            Trans::DotGuard { .. } => continue, // guard fails
                            other => match byte_step(other, c, sep, dot_mask) {
                                Some(n) => (nxt, n),
                                None => continue,
                            },
                        };
                        let base = (n as usize) * nw;
                        for j in 0..nw {
                            buf[target + j] |= closures[base + j];
                        }
                    }
                }
                if !found_new_work {
                    break;
                }
            }

            buf.copy_within(nxt..nxt + nw, cur);
            at_seg_start = sep;
            if buf[cur..cur + nw].iter().all(|&w| w == 0) {
                break;
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

/// Iterate set bits of `bits` as flat NFA-state indices.
///
/// For each word, repeatedly extracts the lowest set bit via
/// `trailing_zeros` and clears it with `word &= word - 1`, so the
/// per-word work scales with `popcount` rather than 64.
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

/// Apply the byte test of one NFA leaf state `t` against `c`.
/// Returns the successor state id if the transition fires, else
/// `None`. The caller owns ε-closure expansion of the result.
///
/// `DotGuard` is intentionally returned as `None` here — its
/// byte-conditional ε action writes to the *current* slot (not
/// next), and is handled inline in [`PikeVm::run_dot_guard`]. Match
/// / Split / Jump never appear in the active set after ε-closure
/// (closures filter to leaves: byte-consumers + DotGuard + Match),
/// and Match doesn't fire on any byte.
///
/// The DFA's
/// [`super::thompson_dfa::collect_successors_bits`] is the same
/// per-byte transition relation, but written as a bitmap-deposit
/// recursion that walks Split/Jump/passing-DotGuard inline (subset
/// construction does the ε-extension on the fly rather than relying
/// on a precomputed `static_closures` table).
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
