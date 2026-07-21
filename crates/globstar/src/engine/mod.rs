//! Compiled-engine module hierarchy.
//!
//! - [`ops`] — `Op` enum and AST→linear lowering, shared by all matchers.
//! - [`facts`] — compile-time literal prefix/suffix facts for fast pre-filter.
//! - [`literal`] — Tier 0 pure-literal matcher.
//! - [`thompson`] — Thompson NFA with explicit ε-transitions and a
//!   [`thompson::Trans::DotGuard`] state for glob's segment-start dot rule.
//!   Built once and shared by the two consumers below.
//! - [`thompson_dfa`] — subset-constructed DFA over Thompson NFA.
//!   Primary Tier 1/2 engine — ~1-2 ns/byte hot loop, aligned with
//!   `regex-automata`'s hybrid DFA and `globset`/`wax` in measurements.
//! - [`pikevm`] — linear-time Pike VM over the Thompson NFA. Fallback
//!   when `thompson_dfa` subset construction exceeds its state cap.

pub mod facts;
pub(crate) mod fxhash;
pub mod literal;
pub mod ops;
pub mod pikevm;
pub(crate) mod thompson;
pub mod thompson_dfa;

/// Per-byte equality with compile-time choice of strict vs. ASCII-case-fold.
///
/// The const-generic `CI` lets each call site (`eq_byte::<true>` vs.
/// `eq_byte::<false>`) monomorphize into a single instruction — the
/// `if CI { ... } else { ... }` branch is dead-code-eliminated at MIR.
#[inline(always)]
pub fn eq_byte<const CI: bool>(a: u8, b: u8) -> bool {
    if CI {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}
