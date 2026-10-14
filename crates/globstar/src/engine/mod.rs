//! Compiled-engine module hierarchy.
//!
//! - `ops` — `Op` enum and the AST → ops lowering the segment engine
//!   compiles from.
//! - `literal` — the pure-literal matcher.
//! - `segment` — the segment-structured matcher for every other
//!   pattern.

pub(crate) mod literal;
pub mod ops;
pub(crate) mod segment;
