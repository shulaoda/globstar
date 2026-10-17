//! Compiled-engine module hierarchy.
//!
//! - `literal` — the pure-literal matcher.
//! - `segment` — the segment-structured matcher for every other
//!   pattern.

pub(crate) mod literal;
pub(crate) mod segment;
