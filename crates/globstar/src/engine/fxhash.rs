//! Minimal FxHash implementation backing [`FxHashMap`].
//!
//! FxHash is a small, fast, non-cryptographic hash designed for
//! integer-keyed maps. Originally implemented for `rustc-hash` /
//! `firefox`'s code; the algorithm is a `rotl(5) ^ word * SEED`
//! running mix, repeated per consumed word.
//!
//! We use it to replace `std`'s SipHash on the subset-construction
//! dedup map ([`FxHashMap<StateKey, DfaStateId>`]) and the byte-class
//! dedup map ([`FxHashMap<(u64, bool, bool), u8>`]) — both keys are
//! small fixed-size words where SipHash's 8-byte-block setup
//! dominates the actual hashing. Profiling showed FxHash dropped
//! [`super::thompson_dfa::ThompsonDfa::build`] time by 12-18% across
//! the compare-bench corpus.
//!
//! Vendored rather than depending on `rustc-hash` to keep the crate
//! dep-free. The algorithm has been stable since 2014; if upstream
//! ever changes it, ours stays the same — a feature, since we treat
//! the hash as a build-time implementation detail (not a stable
//! output anyone depends on).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Multiplier from the original FxHash. `0x5_17c_c1b7_2722_0a95`
/// in standard notation; chosen for good avalanche + cheap
/// pipelined `mul`. Matches what `rustc_hash` ships.
const SEED: u64 = 0x517c_c1b7_2722_0a95;
const ROTATE: u32 = 5;

/// Single-state FxHasher. Zero-initialized state via
/// [`Default`], no allocation.
#[derive(Default, Clone)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = self.hash.rotate_left(ROTATE) ^ w;
        self.hash = self.hash.wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        // Fold full u64 chunks first; this is what dominates on
        // our keys (StateKey's `Vec<u32>`, packed pair tuples).
        while let Some((chunk, rest)) = bytes.split_first_chunk::<8>() {
            self.add(u64::from_ne_bytes(*chunk));
            bytes = rest;
        }
        if let Some((chunk, rest)) = bytes.split_first_chunk::<4>() {
            self.add(u32::from_ne_bytes(*chunk) as u64);
            bytes = rest;
        }
        if let Some((chunk, rest)) = bytes.split_first_chunk::<2>() {
            self.add(u16::from_ne_bytes(*chunk) as u64);
            bytes = rest;
        }
        if let Some(b) = bytes.first() {
            self.add(*b as u64);
        }
    }

    // Direct-int paths skip the byte-decompose loop above. The
    // `Hash` derive on `(u64, bool, bool)` and on `Vec<u32>` lands
    // here for every word — measurable speedup vs the byte path.
    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(n as u64);
    }
    #[inline]
    fn write_u16(&mut self, n: u16) {
        self.add(n as u64);
    }
    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.add(n as u64);
    }
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }
}

/// Drop-in replacement for `std::collections::HashMap` keyed by
/// [`FxHasher`] instead of `RandomState` / SipHash. Use
/// `FxHashMap::with_capacity_and_hasher(cap, Default::default())`
/// to construct (the bare `with_capacity` only exists on
/// `RandomState`-keyed `HashMap`).
pub type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
