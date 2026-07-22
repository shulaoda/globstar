//! DFA compiled via subset construction from a [`Thompson`] NFA.
//!
//! Provides the same hot-loop shape as `regex-automata`'s hybrid DFA —
//! `state = transitions[state << shift | class_of_byte[c]]`, DEAD on
//! mismatch, accepting lookup at end — but built eagerly at compile
//! time with a fixed state cap. The goal is `~1-2 ns/byte` matching,
//! on par with globset / wax on patterns this DFA can build.
//!
//! # Relationship to the other engines
//!
//! - [`super::pikevm::PikeVm`]: linear-time NFA simulator, always
//!   works but ~30 ns/byte. Used as a fallback when DFA construction
//!   would exceed [`MAX_DFA_STATES`].
//!
//! # DFA state encoding
//!
//! A DFA state is `(sorted Vec<NfaStateId>, at_segment_start: bool)` —
//! the NFA subset plus the segment-boundary flag so dot-protection
//! dispatches correctly.
//!
//! # ε-closure vs DotGuard
//!
//! Unconditional ε-transitions ([`Trans::Split`] / [`Trans::Jump`]) are
//! expanded during ε-closure. [`Trans::DotGuard`] is *conditional* on
//! `(at_segment_start, next_byte)`, so it is kept as an NFA state in
//! the DFA state set and evaluated during transition computation — the
//! same late-binding the Pike VM applies, just materialized in a DFA
//! table rather than re-computed per call.

use crate::dir_match::DirMatch;
use crate::engine::facts::LiteralFacts;
use crate::engine::fxhash::FxHashMap;
use crate::engine::ops::{OpProgram, compute_static_prefixes};
use crate::engine::thompson::{StateId as NfaStateId, Thompson, Trans, compute_static_closures};

/// Hard cap on DFA state count. Patterns whose subset construction
/// would exceed this fall back to [`super::pikevm::PikeVm`]. Chosen to
/// match `regex-automata`'s "state count that still fits comfortably
/// in cache" — 4096 × 16 classes × 2 B = 128 KB transition table in
/// the worst case, well under typical L2.
pub const MAX_DFA_STATES: usize = 4096;

type DfaStateId = u16;

/// Dead state id. `transitions[DEAD * stride | ..] == DEAD` (self-
/// loop) and `accepting[DEAD] == false`, so the hot loop just
/// checks `state == 0` as its termination condition.
const DEAD: DfaStateId = 0;

/// Compiled DFA.
#[derive(Debug, Clone)]
pub struct ThompsonDfa {
    /// Row-major: `transitions[(state as usize << stride_shift) | class]`.
    transitions: Box<[DfaStateId]>,
    /// Byte → byte-class id. 256 entries so the hot loop indexes
    /// directly by the input byte with no bounds check in release.
    class_of_byte: [u8; 256],
    /// `log2(stride)`. Row width = `1 << stride_shift`, always a power
    /// of 2 so `state * stride + class` collapses to `state << shift |
    /// class`. Tried IMUL-stride (~25-30% slower on Apple Silicon — IMUL
    /// latency dominated the cache saving) and a `state * 256 + byte`
    /// fused table (~4-6× heap on monster patterns for only 1-6% speedup
    /// on real walker patterns); both reverted.
    stride_shift: u8,
    /// `accepting[state]`.
    accepting: Box<[bool]>,
    /// Initial DFA state (always 1; 0 is DEAD).
    initial: DfaStateId,
    /// Per-state "can we reach accept via some non-empty byte sequence".
    /// Populated via reverse BFS from accepting states. Used for
    /// [`ThompsonDfa::match_dir`] Prefix-mode.
    reach_to_accept: Box<[bool]>,
    /// Suffix/prefix literal pre-filter — short-circuits `is_match`
    /// before the byte loop. Extracted from the compile-time
    /// [`OpProgram`] and kept; the rest of the program is dropped
    /// after the DFA is built so its `Op::Alternation` tree (which
    /// can reach several KB on wide brace patterns) doesn't bloat
    /// every retained matcher.
    facts: LiteralFacts,
    /// Pre-computed static path prefixes for walker integration.
    /// Cached here at build time so the upstream `Glob::static_prefixes`
    /// call is a no-op clone — the `OpProgram` that produced these
    /// is no longer retained.
    prefixes: Box<[Box<[u8]>]>,
}

impl ThompsonDfa {
    /// Try to build a DFA from `program`. On state-cap overflow
    /// returns `program` back via `Err` so the caller can move it
    /// into a fallback engine without re-parsing / re-lowering.
    /// Compiles the intermediate Thompson NFA internally.
    pub fn build(program: OpProgram, dot: bool) -> Result<Box<Self>, OpProgram> {
        let thompson = Thompson::compile(&program, dot);
        let (class_of_byte, num_classes, tracks_seg_start) = build_byte_classes(&thompson);
        let stride_shift = class_stride_shift(num_classes as usize);
        let stride = 1usize << stride_shift;

        let nfa_state_count = thompson.states.len();
        let n_words = nfa_state_count.div_ceil(64);

        // Pre-compute accepts_at_eof as a bitmap so `accepts` checks
        // become a single AND over `n_words` words.
        let mut eof_bits = vec![0u64; n_words].into_boxed_slice();
        for (i, &eof) in thompson.accepts_at_eof.iter().enumerate() {
            if eof {
                eof_bits[i >> 6] |= 1u64 << (i & 63);
            }
        }

        // Pre-compute per-state ε-closures (Split/Jump only — DotGuard
        // is a leaf, byte-conditional). Memoized recursion over the
        // ε-graph; Thompson construction guarantees the graph is
        // acyclic at the byte-consumer / DotGuard / Match boundary.
        let static_closures = compute_static_closures(&thompson, n_words);

        let mut builder = Builder {
            thompson: &thompson,
            class_of_byte: &class_of_byte,
            num_classes,
            stride,
            stride_shift,
            n_words,
            eof_bits,
            static_closures,
            tracks_seg_start,
            state_map: FxHashMap::with_capacity_and_hasher(32, Default::default()),
            states: Vec::with_capacity(32),
            transitions: Vec::with_capacity(stride * 32),
            accepting: Vec::with_capacity(32),
            queue: Vec::new(),
            scratch_bits: vec![0u64; n_words],
            closure_temp: vec![0u64; n_words],
        };
        builder.seed_dead();
        let Some(initial) = builder.intern_initial() else {
            return Err(program);
        };
        if builder.run(initial).is_none() {
            return Err(program);
        }

        let reach_to_accept = compute_reach_to_accept(
            builder.states.len(),
            &builder.transitions,
            stride_shift,
            &builder.accepting,
        );

        // Cache what callers need from `program` (the prefilter
        // facts and the walker prefixes), then drop the rest of the
        // op tree — for wide brace patterns the `Op::Alternation`
        // subtree alone can be 4-6 KB, dwarfing the DFA tables.
        let prefixes = compute_static_prefixes(program.ops());
        let (_, facts, _) = program.into_parts();

        Ok(Box::new(Self {
            transitions: builder.transitions.into_boxed_slice(),
            class_of_byte,
            stride_shift,
            accepting: builder.accepting.into_boxed_slice(),
            initial,
            reach_to_accept: reach_to_accept.into_boxed_slice(),
            facts,
            prefixes,
        }))
    }

    /// Cached static path prefixes for walker integration — see
    /// [`crate::Glob::static_prefixes`].
    pub fn static_prefixes(&self) -> &[Box<[u8]>] {
        &self.prefixes
    }

    /// Diagnostic: number of distinct byte classes this DFA uses (≤ stride).
    pub fn num_byte_classes(&self) -> usize {
        // stride = 1 << stride_shift is an upper bound; actual class count
        // is min(stride, number of unique values in class_of_byte + 1).
        let mut seen = [false; 256];
        let mut count = 0usize;
        for &c in self.class_of_byte.iter() {
            if !seen[c as usize] {
                seen[c as usize] = true;
                count += 1;
            }
        }
        count
    }

    /// Diagnostic: number of DFA states.
    pub fn num_states(&self) -> usize {
        self.accepting.len()
    }

    /// Indexed lookup `transitions[state, class]`. Sole site computing
    /// the `state << shift | class` row offset — keeps `is_match` /
    /// `match_dir` / `compute_reach_to_accept` consistent and lets the
    /// optimizer hoist `stride_shift` once across the byte loop.
    #[inline(always)]
    fn step(&self, state: DfaStateId, class: u8) -> DfaStateId {
        self.transitions[((state as usize) << self.stride_shift) | (class as usize)]
    }

    /// Full-match query. Mirrors [`super::pikevm::PikeVm::is_match`]
    /// semantics — same facts pre-filter, same DEAD short-circuit.
    ///
    /// The per-byte `if state == DEAD { return false }` early exit is
    /// intentionally omitted: DEAD is an absorbing self-loop with
    /// `accepting[DEAD] == false`, so letting the loop run to the end
    /// produces the correct result either way. Facts pre-filter catches
    /// most structural mismatches before this point.
    #[inline(always)]
    pub fn is_match(&self, path: &[u8]) -> bool {
        if !self.facts.accept(path) {
            return false;
        }
        let mut state = self.initial;
        for &c in path {
            state = self.step(state, self.class_of_byte[c as usize]);
        }
        self.accepting[state as usize]
    }

    /// Walker-style directory query. Hypothetical `/` step followed by
    /// a reach-to-accept check — mirrors
    /// [`super::pikevm::PikeVm::match_dir`].
    ///
    /// Unlike [`Self::is_match`], this keeps the per-byte DEAD early
    /// exit: walker callers rely on it to prune subtrees without
    /// finishing the traversal.
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        let mut state = self.initial;
        for &c in dir_path {
            state = self.step(state, self.class_of_byte[c as usize]);
            if state == DEAD {
                return DirMatch::Pruned;
            }
        }
        let exact = self.accepting[state as usize];

        // Hypothetical descendant '/': if consuming a separator leaves
        // us in a state that either accepts or can reach accept, a
        // descendant exists.
        let after_sep = self.step(state, self.class_of_byte[b'/' as usize]);
        let prefix = after_sep != DEAD
            && (self.accepting[after_sep as usize] || self.reach_to_accept[after_sep as usize]);

        DirMatch::from_exact_prefix(exact, prefix)
    }
}

/// Return `log2(next_power_of_two(num_classes))`. Used to collapse
/// `state * stride + class` to `state << shift | class`.
fn class_stride_shift(num_classes: usize) -> u8 {
    let stride = num_classes.next_power_of_two().max(2);
    stride.trailing_zeros() as u8
}

/// Compute a byte-class table for `thompson`. Two bytes share a
/// class iff every NFA state accepts/rejects them identically, modulo
/// the `is_sep`/`is_dot` key-narrowing flags. Returns
/// `(class_of_byte, num_classes, tracks_seg_start)`.
///
/// `tracks_seg_start` is true iff some NFA state has `dot_protected:
/// true` and therefore reads `at_segment_start`. When false, callers
/// drop that dimension from `StateKey`, collapsing
/// otherwise-equivalent DFA states.
///
/// # Per-state acceptance criteria
///
/// - [`Trans::Byte`] — byte equals `b`.
/// - [`Trans::Class`] — `class.matches(c)`. The dot-protection gate
///   fires at runtime, keyed by `is_dot`.
/// - [`Trans::AnyNonSep`] — `!is_separator(c)`, modulo dot rule.
/// - [`Trans::AnyByte`] — any byte, modulo dot rule.
/// - [`Trans::Sep`] — `is_separator(c)`.
///
/// # Algorithm — bitmap inversion
///
/// The straightforward `for c in 0..256 { for state in states { … }
/// }` shape was profiled at ~40% of `ThompsonDfa::build` total on
/// brace-heavy patterns, with 256 per-byte `Vec<NfaStateId>`
/// allocations and 256 set hashes dominating. The current code
/// inverts the loops:
///
/// 1. **Per-state accept mask**: for each NFA state, build a 256-bit
///    bitmap of the bytes it accepts. Only `Trans::Class` /
///    `Trans::AnyNonSep` / `Trans::Sep` are O(256) (the latter two
///    via inline `std::path::is_separator` calls — the function is
///    a tiny inlined match that LLVM unrolls cleanly).
/// 2. **Transpose**: for each NFA state `s`, OR `1 << s` into
///    `signatures[c]` for every set bit `c` in the state's mask, by
///    iterating set bits with `trailing_zeros` (so the work scales
///    with `popcount`, not with 256).
/// 3. **Dedup**: keyed by `(u64, bool, bool)` when the NFA has ≤ 64
///    states (the common case), otherwise by `(Vec<u64>, bool, bool)`.
///    Either way: no per-byte heap allocation, and `u64` hashing
///    instead of unbounded set hashing.
fn build_byte_classes(thompson: &Thompson) -> ([u8; 256], u8, bool) {
    let states = &thompson.states;
    let n = states.len();

    // Phase 1 — narrow the class-key discriminators to what some
    // state actually reads. If no NFA state is `Sep` / `AnyNonSep` /
    // `AnyByte`, separator-ness doesn't change any transition, so
    // keying on it just inflates the class count. Same for
    // dot-protection: only matters if at least one state has
    // `dot_protected: true`. For `{a..h} × K` this drops 2 redundant
    // classes (sep, dot) that both would have collapsed to the same
    // DEAD column anyway, getting us from 11 → 9 effective classes.
    let mut key_sep = false;
    let mut key_dot = false;
    for t in states {
        match t {
            Trans::Sep { .. } => key_sep = true,
            Trans::AnyNonSep { dot_protected, .. } => {
                key_sep = true;
                if *dot_protected {
                    key_dot = true;
                }
            }
            Trans::AnyByte { dot_protected, .. } | Trans::Class { dot_protected, .. } => {
                if *dot_protected {
                    key_dot = true;
                }
            }
            _ => {}
        }
    }
    // Any live dot-protection means the DFA's StateKey tracks
    // `at_segment_start`, which flips based on whether the consumed
    // byte was a separator. A class that lumps `/` in with non-sep
    // bytes would route both to the same DFA successor — wrong,
    // because a real `/` must land in an at-segment-start state so
    // the next byte's dot-protection fires. Force sep-vs-not
    // distinction whenever dot-protection is in play (e.g. `**`
    // alone: only AnyByte states, no Sep/AnyNonSep, key_sep would
    // otherwise be false and `a/.hidden` would spuriously match).
    if key_dot {
        key_sep = true;
    }

    // Pre-compute platform-fixed masks once instead of running a
    // 256-byte loop per AnyNonSep / Sep / AnyByte state. For typical
    // single-pattern compiles this drops phase-2 cost from ~512 ns
    // to ~30 ns.
    let mut sep_mask = [0u64; 4];
    let mut non_sep_mask = [0u64; 4];
    for c in 0..=255u8 {
        if std::path::is_separator(c as char) {
            set_bit(&mut sep_mask, c);
        } else {
            set_bit(&mut non_sep_mask, c);
        }
    }
    let all_mask = [u64::MAX; 4];

    // Phase 2 — for each NFA state, compute its 256-bit accept mask.
    // Stored as `[u64; 4]` per state.
    let mut accept_masks: Vec<[u64; 4]> = vec![[0u64; 4]; n];
    for (id, trans) in states.iter().enumerate() {
        let mask = &mut accept_masks[id];
        match trans {
            Trans::Byte { b, .. } => set_bit(mask, *b),
            Trans::Class { class, .. } => {
                for c in 0..=255u8 {
                    if class.matches(c) {
                        set_bit(mask, c);
                    }
                }
            }
            Trans::AnyNonSep { .. } => *mask = non_sep_mask,
            Trans::AnyByte { .. } => *mask = all_mask,
            Trans::Sep { .. } => *mask = sep_mask,
            _ => {} // Split, Jump, DotGuard, Match — no byte accepted
        }
    }

    if n <= 64 {
        build_byte_classes_u64(&accept_masks, key_sep, key_dot)
    } else {
        build_byte_classes_vec(&accept_masks, key_sep, key_dot, n)
    }
}

#[inline]
fn set_bit(mask: &mut [u64; 4], bit: u8) {
    mask[(bit / 64) as usize] |= 1u64 << (bit % 64);
}

/// Open-addressed `(sig, is_sep, is_dot) → class_id` table used by
/// [`build_byte_classes_u64`]. Stack-allocated, no per-byte heap
/// alloc; replaces an `FxHashMap` whose per-byte entry-or-insert
/// cost dominated phase 3 of compile (~3 µs of a 4 µs solo build).
///
/// Sized 64 because distinct byte classes for typical patterns sit
/// at 5-16 (measured across our 7-pattern bench), keeping load
/// factor under 25% and probe depth at 1-2.
struct ClassTable {
    keys: [(u64, u8, u8); Self::CAP],
    /// Per-slot class id; `u8::MAX` marks the slot empty.
    slot_class: [u8; Self::CAP],
    num_classes: u8,
}

impl ClassTable {
    const CAP: usize = 64;
    const MASK: usize = Self::CAP - 1;

    /// Compile-time invariant: every storable `num_classes` must
    /// fit in the `u8` space minus the `u8::MAX` empty-slot
    /// sentinel. Trivially holds at CAP=64; this static check is
    /// what protects future bumps.
    const _ASSERT: () = assert!(Self::CAP <= 255, "CAP must leave room for u8::MAX sentinel");

    fn new() -> Self {
        Self {
            keys: [(0, 0, 0); Self::CAP],
            slot_class: [u8::MAX; Self::CAP],
            num_classes: 0,
        }
    }

    /// Look up `(sig, is_sep, is_dot)`. Returns the existing class
    /// id on hit, a freshly minted id on first miss, or `None` if
    /// the table is full (caller falls back to the HashMap path).
    #[inline]
    fn lookup_or_insert(&mut self, sig: u64, is_sep: u8, is_dot: u8) -> Option<u8> {
        // Mix sig + flags into a starting probe. Low bits of sig are
        // typically well-distributed (driven by which NFA states fire
        // on `c`); the flag XOR ensures (sep, dot) variants of the
        // same sig spread across adjacent slots.
        let mut slot = ((sig as usize)
            ^ ((sig >> 32) as usize)
            ^ ((is_sep as usize) << 1)
            ^ (is_dot as usize))
            & Self::MASK;

        loop {
            if self.slot_class[slot] == u8::MAX {
                // Reserve one slot so probes always have an empty
                // landing spot — guarantees termination on miss.
                if self.num_classes as usize >= Self::CAP - 1 {
                    return None;
                }
                self.keys[slot] = (sig, is_sep, is_dot);
                self.slot_class[slot] = self.num_classes;
                let id = self.num_classes;
                self.num_classes += 1;
                return Some(id);
            }
            if self.keys[slot] == (sig, is_sep, is_dot) {
                return Some(self.slot_class[slot]);
            }
            slot = (slot + 1) & Self::MASK;
        }
    }
}

/// Fast path for NFAs with ≤ 64 states: per-byte signature fits in a
/// single `u64`, and the dedup map keys on `(u64, bool, bool)`. No
/// per-byte heap allocation.
fn build_byte_classes_u64(
    accept_masks: &[[u64; 4]],
    key_sep: bool,
    key_dot: bool,
) -> ([u8; 256], u8, bool) {
    // Transpose: `signatures[c]` is the bitmap of NFA states that
    // accept byte `c`. Iterate set bits of each accept_mask so the
    // work is `O(sum of popcount)` rather than `O(256 × n)`.
    let mut signatures: [u64; 256] = [0u64; 256];
    for (s, mask) in accept_masks.iter().enumerate() {
        let bit = 1u64 << (s as u32);
        for (word_idx, &word_value) in mask.iter().enumerate() {
            let mut w = word_value;
            let base = word_idx * 64;
            while w != 0 {
                let off = w.trailing_zeros() as usize;
                signatures[base + off] |= bit;
                w &= w - 1;
            }
        }
    }

    // Pre-compute (is_sep, is_dot) LUTs once instead of branching per
    // byte. `key_sep` / `key_dot` decide if either dimension is even
    // tracked — a `false` skips the byte test and forces the LUT
    // bit to 0, collapsing the dedup key.
    let mut sep_lut = [0u8; 256];
    let mut dot_lut = [0u8; 256];
    if key_sep {
        for c in 0..=255u8 {
            sep_lut[c as usize] = std::path::is_separator(c as char) as u8;
        }
    }
    if key_dot {
        dot_lut[b'.' as usize] = 1;
    }

    let mut table = ClassTable::new();
    let mut class_of_byte = [0u8; 256];
    for c in 0..=255u8 {
        match table.lookup_or_insert(
            signatures[c as usize],
            sep_lut[c as usize],
            dot_lut[c as usize],
        ) {
            Some(id) => class_of_byte[c as usize] = id,
            None => {
                return build_byte_classes_u64_fallback(&signatures, &sep_lut, &dot_lut, key_dot);
            }
        }
    }
    (class_of_byte, table.num_classes, key_dot)
}

/// Cold fallback for pattern sets that overflow [`ClassTable::CAP`]
/// distinct classes — keep the original HashMap path for
/// correctness. Hasn't fired on any real corpus we test.
#[cold]
fn build_byte_classes_u64_fallback(
    signatures: &[u64; 256],
    sep_lut: &[u8; 256],
    dot_lut: &[u8; 256],
    key_dot: bool,
) -> ([u8; 256], u8, bool) {
    let mut key_to_class: FxHashMap<(u64, bool, bool), u8> =
        FxHashMap::with_capacity_and_hasher(64, Default::default());
    let mut class_of_byte = [0u8; 256];
    for c in 0..=255u8 {
        let is_sep = sep_lut[c as usize] != 0;
        let is_dot = dot_lut[c as usize] != 0;
        let key = (signatures[c as usize], is_sep, is_dot);
        let next_id = key_to_class.len() as u8;
        let class_id = *key_to_class.entry(key).or_insert(next_id);
        class_of_byte[c as usize] = class_id;
    }
    (class_of_byte, key_to_class.len() as u8, key_dot)
}

/// Fallback for NFAs with > 64 states: same algorithm but the
/// per-byte signature is a `Vec<u64>` of `ceil(n / 64)` words.
fn build_byte_classes_vec(
    accept_masks: &[[u64; 4]],
    key_sep: bool,
    key_dot: bool,
    n: usize,
) -> ([u8; 256], u8, bool) {
    let words = n.div_ceil(64);
    let mut signatures: Vec<Vec<u64>> = (0..256).map(|_| vec![0u64; words]).collect();
    for (s, mask) in accept_masks.iter().enumerate() {
        let word_idx_in_sig = s / 64;
        let bit = 1u64 << ((s % 64) as u32);
        for (w_idx, &word_value) in mask.iter().enumerate() {
            let mut bits = word_value;
            let base = w_idx * 64;
            while bits != 0 {
                let off = bits.trailing_zeros() as usize;
                signatures[base + off][word_idx_in_sig] |= bit;
                bits &= bits - 1;
            }
        }
    }

    let mut key_to_class: FxHashMap<(Vec<u64>, bool, bool), u8> =
        FxHashMap::with_capacity_and_hasher(16, Default::default());
    let mut class_of_byte = [0u8; 256];
    for c in 0..=255u8 {
        let is_sep = key_sep && std::path::is_separator(c as char);
        let is_dot = key_dot && c == b'.';
        let key = (std::mem::take(&mut signatures[c as usize]), is_sep, is_dot);
        let next_id = key_to_class.len() as u8;
        let class_id = *key_to_class.entry(key).or_insert(next_id);
        class_of_byte[c as usize] = class_id;
    }
    (class_of_byte, key_to_class.len() as u8, key_dot)
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct StateKey {
    /// Active NFA-state bitmap, `n_words` u64s. Bit `s` set ⇔ state
    /// `s` is in the active set after ε-closure of Split/Jump (but
    /// NOT DotGuard — that's resolved per-byte). A bitmap key beats
    /// `Vec<NfaStateId>` for the merged-pattern case (`Glob::union`
    /// with many branches): hash + equality become O(n_words) word
    /// ops instead of O(active.len()) elementwise — typically 5-15×
    /// fewer ops, plus the data is contiguous and cache-friendly.
    bits: Box<[u64]>,
    /// True at position 0 or immediately after a separator byte.
    /// Determines whether dot-protection gates fire.
    at_segment_start: bool,
}

struct Builder<'a> {
    thompson: &'a Thompson,
    class_of_byte: &'a [u8; 256],
    num_classes: u8,
    stride: usize,
    stride_shift: u8,
    /// `ceil(thompson.states.len() / 64)`. All `bits` slices and
    /// `scratch_bits` share this length.
    n_words: usize,
    /// Pre-computed `accepts_at_eof` bitmap so `accepts` checks are
    /// one AND across `n_words` instead of an O(active.len()) scan.
    eof_bits: Box<[u64]>,
    /// Pre-computed per-NFA-state ε-closure bitmap. `static_closures
    /// [s * n_words .. (s+1) * n_words]` is the bitmap of leaf states
    /// (byte-consumers + DotGuard + Match) reachable from state `s`
    /// via Split/Jump only. Computed once at builder construction;
    /// replaces the per-step dynamic BFS in [`epsilon_closure_bits`]
    /// with O(active × n_words) bitmap OR. Big win on merged-pattern
    /// unions where each step's active set has many states whose
    /// ε-closures used to require reachable-via-Split/Jump BFS.
    static_closures: Vec<u64>,
    /// When `false`, no NFA state reads `at_segment_start`, so the
    /// `StateKey::at_segment_start` dimension can be forced to a
    /// constant — DFA states that only differed in that flag collapse
    /// into one. Halves the DFA state count for the common
    /// `dot=true` (default) compile path, where every dot-protection
    /// gate is a no-op.
    tracks_seg_start: bool,
    state_map: FxHashMap<StateKey, DfaStateId>,
    states: Vec<StateKey>,
    transitions: Vec<DfaStateId>,
    accepting: Vec<bool>,
    queue: Vec<DfaStateId>,
    /// Reusable byte-step successors buffer (active-set bitmap of
    /// length `n_words`). Zeroed at the head of every [`Self::step`]
    /// call rather than reallocated.
    scratch_bits: Vec<u64>,
    /// Reusable closure-expand temp buffer (length `n_words`). Used
    /// by [`epsilon_closure_via_static`] to accumulate the OR of
    /// per-state closures without aliasing the input bitmap.
    closure_temp: Vec<u64>,
}

impl<'a> Builder<'a> {
    fn empty_bits(&self) -> Box<[u64]> {
        vec![0u64; self.n_words].into_boxed_slice()
    }

    /// `accepts[s] & state-bitmap ≠ 0` — one AND over `n_words` u64s.
    fn bitmap_accepts(&self, bits: &[u64]) -> bool {
        for (e, b) in self.eof_bits.iter().zip(bits.iter()) {
            if (*e & *b) != 0 {
                return true;
            }
        }
        false
    }

    fn seed_dead(&mut self) {
        let dead_key = StateKey {
            bits: self.empty_bits(),
            at_segment_start: false,
        };
        self.state_map.insert(dead_key.clone(), DEAD);
        self.states.push(dead_key);
        self.transitions
            .extend(std::iter::repeat_n(DEAD, self.stride));
        self.accepting.push(false);
    }

    fn intern_initial(&mut self) -> Option<DfaStateId> {
        // Bootstrap from the precomputed closure of the initial state
        // — leaf set = static_closures[initial].
        let init_idx = self.thompson.initial as usize;
        let base = init_idx * self.n_words;
        self.scratch_bits
            .copy_from_slice(&self.static_closures[base..base + self.n_words]);
        let accepts_empty = self.bitmap_accepts(&self.scratch_bits);
        let key = StateKey {
            bits: self.scratch_bits.clone().into_boxed_slice(),
            // Initial state is at a segment boundary, but if no NFA
            // state reads the flag, we collapse the dimension so
            // post-`/` and post-non-`/` paths land on the same
            // canonical state without forcing a separate copy of every
            // DFA state for the boundary case.
            at_segment_start: self.tracks_seg_start,
        };
        let id = 1u16;
        self.state_map.insert(key.clone(), id);
        self.states.push(key);
        self.transitions
            .extend(std::iter::repeat_n(DEAD, self.stride));
        self.accepting.push(accepts_empty);
        self.queue.push(id);
        Some(id)
    }

    fn run(&mut self, _initial: DfaStateId) -> Option<()> {
        // Pre-compute a representative byte per class for transition evaluation.
        let mut representative_byte = vec![0u8; self.num_classes as usize];
        for c in 0..=255u8 {
            representative_byte[self.class_of_byte[c as usize] as usize] = c;
        }

        while let Some(id) = self.queue.pop() {
            for class in 0..self.num_classes {
                let byte = representative_byte[class as usize];
                let next_id = self.step(id, byte)?;
                let slot = ((id as usize) << self.stride_shift) | (class as usize);
                self.transitions[slot] = next_id;
            }
        }
        Some(())
    }

    /// Compute the DFA state reached by consuming byte `c` from DFA
    /// state `from_id`. Interns the new state if not seen; returns
    /// `None` on state cap overflow.
    fn step(&mut self, from_id: DfaStateId, byte: u8) -> Option<DfaStateId> {
        let at_seg_start = self.states[from_id as usize].at_segment_start;

        // Re-grow scratch on first step after a miss (where it was
        // mem::take'd) — otherwise just zero out the existing buffer.
        if self.scratch_bits.len() < self.n_words {
            self.scratch_bits.resize(self.n_words, 0);
        } else {
            for w in self.scratch_bits.iter_mut() {
                *w = 0;
            }
        }

        // Iterate set bits in the source bitmap; for each NFA state
        // collect successors directly into scratch_bits.
        for w_idx in 0..self.n_words {
            let mut word = self.states[from_id as usize].bits[w_idx];
            while word != 0 {
                let bit = word.trailing_zeros() as usize;
                let s = (w_idx * 64 + bit) as NfaStateId;
                word &= word - 1;
                collect_successors_bits(
                    self.thompson,
                    s,
                    byte,
                    at_seg_start,
                    &mut self.scratch_bits,
                );
            }
        }

        if self.scratch_bits.iter().all(|&w| w == 0) {
            return Some(DEAD);
        }

        epsilon_closure_via_static(
            &self.static_closures,
            &mut self.scratch_bits,
            &mut self.closure_temp,
        );

        // `next_seg_start` only matters when some NFA state reads the
        // flag (i.e. dot-protection is in play). For the common
        // `dot=true` compile path nothing reads it, so we force
        // `false` to collapse otherwise-equivalent post-`/` and
        // post-non-`/` DFA states.
        let next_seg_start = self.tracks_seg_start && std::path::is_separator(byte as char);

        // Move scratch into the probe key via `mem::take` — zero
        // alloc on the hit path (hits dominate ~95%), one clone-on-
        // insert on miss. `Vec::into_boxed_slice` is alloc-free when
        // len == capacity, which it is here (we resize to exactly
        // n_words). On hit we swap the buffer back so the next
        // step reuses the same allocation; on miss the buffer is
        // consumed and the next step's `resize` re-allocates once.
        let probe = StateKey {
            bits: std::mem::take(&mut self.scratch_bits).into_boxed_slice(),
            at_segment_start: next_seg_start,
        };
        if let Some(&id) = self.state_map.get(&probe) {
            self.scratch_bits = probe.bits.into_vec();
            return Some(id);
        }
        if self.states.len() >= MAX_DFA_STATES {
            return None;
        }
        let new_id = self.states.len() as DfaStateId;
        let accepts = self.bitmap_accepts(&probe.bits);
        // One unavoidable clone: the key needs to live in both the
        // dedup map and the canonical state list.
        self.state_map.insert(probe.clone(), new_id);
        self.states.push(probe);
        self.transitions
            .extend(std::iter::repeat_n(DEAD, self.stride));
        self.accepting.push(accepts);
        self.queue.push(new_id);
        Some(new_id)
    }
}

/// Set bit `s` in `bits`. Caller guarantees `s < n_words * 64`.
#[inline]
fn set_state_bit(bits: &mut [u64], s: NfaStateId) {
    let idx = (s as usize) >> 6;
    let bit = (s as usize) & 63;
    bits[idx] |= 1u64 << bit;
}

/// Expand ε-closure on a bitmap using pre-computed `static_closures`.
/// For each set bit `s` in `bits`, OR `static_closures[s]` into
/// `temp`, then copy back into `bits`. O(active × n_words) word ops.
///
/// Replaces a per-step BFS over Split/Jump edges with a single sweep
/// of OR ops — for merged-pattern unions, where each active set spans
/// many states reachable via wide ε-graphs, the savings are large
/// (~20-30% of total compile time for `huge-set-pos`).
///
/// `temp.len() == bits.len() == n_words`. Caller pre-allocates so we
/// never alloc inside.
fn epsilon_closure_via_static(static_closures: &[u64], bits: &mut [u64], temp: &mut [u64]) {
    let n_words = bits.len();
    for w in temp.iter_mut() {
        *w = 0;
    }
    for (w_idx, &word_init) in bits.iter().enumerate() {
        let mut word = word_init;
        while word != 0 {
            let bit = word.trailing_zeros() as usize;
            let s = w_idx * 64 + bit;
            word &= word - 1;
            let base = s * n_words;
            // Unrolled by hand for the common n_words cases (1..=4
            // covers up to 256 NFA states); the generic loop handles
            // the long tail.
            match n_words {
                1 => temp[0] |= static_closures[base],
                2 => {
                    temp[0] |= static_closures[base];
                    temp[1] |= static_closures[base + 1];
                }
                3 => {
                    temp[0] |= static_closures[base];
                    temp[1] |= static_closures[base + 1];
                    temp[2] |= static_closures[base + 2];
                }
                4 => {
                    temp[0] |= static_closures[base];
                    temp[1] |= static_closures[base + 1];
                    temp[2] |= static_closures[base + 2];
                    temp[3] |= static_closures[base + 3];
                }
                _ => {
                    for j in 0..n_words {
                        temp[j] |= static_closures[base + j];
                    }
                }
            }
        }
    }
    bits.copy_from_slice(temp);
}

/// Apply byte `c` from NFA state `s`, depositing successors (with
/// ε-closure via Split/Jump, and DotGuard evaluated against
/// `at_segment_start && c == b'.'`) into `out_bits` as set bits.
///
/// Mirrors [`super::pikevm::byte_step`] semantics, with two
/// differences: (1) deposits successors into a bitmap rather than
/// returning a single `Option<StateId>`, and (2) recurses through
/// `Split` / `Jump` / passing `DotGuard` so subset construction
/// captures the full ε-extended successor set in one pass (the
/// PikeVM relies on its precomputed `static_closures` for that
/// instead).
fn collect_successors_bits(
    thompson: &Thompson,
    s: NfaStateId,
    c: u8,
    at_segment_start: bool,
    out_bits: &mut [u64],
) {
    match &thompson.states[s as usize] {
        Trans::Byte { b, next } => {
            if *b == c {
                set_state_bit(out_bits, *next);
            }
        }
        Trans::Class {
            class,
            next,
            dot_protected,
        } => {
            if class.matches(c) && !(*dot_protected && at_segment_start && c == b'.') {
                set_state_bit(out_bits, *next);
            }
        }
        Trans::AnyNonSep {
            next,
            dot_protected,
        } => {
            if !(std::path::is_separator(c as char)
                || *dot_protected && at_segment_start && c == b'.')
            {
                set_state_bit(out_bits, *next);
            }
        }
        Trans::AnyByte {
            next,
            dot_protected,
        } => {
            if !(*dot_protected && at_segment_start && c == b'.') {
                set_state_bit(out_bits, *next);
            }
        }
        Trans::Sep { next } => {
            if std::path::is_separator(c as char) {
                set_state_bit(out_bits, *next);
            }
        }
        Trans::DotGuard { next } => {
            if !(at_segment_start && c == b'.') {
                // Guard passes — recurse so Split/Jump/chained DotGuard
                // beyond this gate are traced using the same byte.
                collect_successors_bits(thompson, *next, c, at_segment_start, out_bits);
            }
        }
        Trans::Split { a, b } => {
            collect_successors_bits(thompson, *a, c, at_segment_start, out_bits);
            collect_successors_bits(thompson, *b, c, at_segment_start, out_bits);
        }
        Trans::Jump { next } => {
            collect_successors_bits(thompson, *next, c, at_segment_start, out_bits);
        }
        Trans::Match => {
            // Match has no outgoing transitions.
        }
    }
}

/// Backwards BFS: mark every DFA state that can reach a non-self
/// accepting state via one or more byte-consuming transitions. Used
/// by [`ThompsonDfa::match_dir`] to answer Prefix-mode queries.
fn compute_reach_to_accept(
    num_states: usize,
    transitions: &[DfaStateId],
    stride_shift: u8,
    accepting: &[bool],
) -> Vec<bool> {
    let n = num_states;
    let mut rev: Vec<Vec<DfaStateId>> = vec![Vec::new(); n];
    let stride = 1usize << stride_shift;
    for from in 0..n {
        for class in 0..stride {
            let slot = (from << stride_shift) | class;
            if slot >= transitions.len() {
                break;
            }
            let to = transitions[slot];
            if to == DEAD {
                continue;
            }
            rev[to as usize].push(from as DfaStateId);
        }
    }
    let mut reach = vec![false; n];
    let mut stack: Vec<DfaStateId> = Vec::new();
    for id in 0..n {
        if accepting[id] {
            for &prev in &rev[id] {
                if !reach[prev as usize] {
                    reach[prev as usize] = true;
                    stack.push(prev);
                }
            }
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
