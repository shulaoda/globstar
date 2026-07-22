//! Compiled-engine module hierarchy.
//!
//! - [`ops`] — `Op` enum and AST→linear lowering, shared by all matchers.
//! - [`facts`] — compile-time literal suffix facts for fast pre-filtering.
//! - [`literal`] — Tier 0 pure-literal matcher.
//! - [`segment`] — primary segment-structured matcher.
//! - [`thompson`] — Thompson NFA compiled for the Pike VM fallback.
//! - [`pikevm`] — total linear-time fallback for patterns outside the
//!   segment engine's bounded representation.

pub mod facts;
pub mod literal;
pub mod ops;
pub mod pikevm;
pub(crate) mod segment;
pub(crate) mod thompson;

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
