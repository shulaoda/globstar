//! Compiled-engine module hierarchy.
//!
//! - `ops` — `Op` enum and AST→linear lowering, shared by all matchers.
//! - `literal` — Tier 0 pure-literal matcher.
//! - `segment` — the segment-structured matcher for every other
//!   pattern.

pub(crate) mod literal;
pub mod ops;
pub(crate) mod segment;
