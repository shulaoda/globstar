//! Shared compiler IR facade.
//!
//! The implementation is split by responsibility while this module keeps the
//! historical `engine::ops::*` import surface stable for matcher backends.

mod ir;
mod lower;
mod normalize;
mod prefixes;

pub use ir::{Op, OpProgram};
pub use lower::{lower, lower_owned};
pub use prefixes::{compute_static_prefixes, extract_prefix};
