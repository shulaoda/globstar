//! Compiled-engine module hierarchy.
//!
//! - `ops` — `Op` enum and AST→linear lowering, shared by all matchers.
//! - `facts` — compile-time literal suffix facts for fast pre-filtering.
//! - `literal` — Tier 0 pure-literal matcher.
//! - `segment` — primary segment-structured matcher.
//! - `thompson` — Thompson NFA compiled for the Pike VM fallback.
//! - `pikevm` — total linear-time fallback for patterns outside the
//!   segment engine's bounded representation.

pub(crate) mod facts;
pub(crate) mod literal;
pub mod ops;
pub mod pikevm;
pub(crate) mod segment;
pub(crate) mod thompson;
