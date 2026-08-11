mod ir;
mod lower;
mod prefixes;

pub use ir::{Op, OpProgram};
pub use lower::lower;
pub(crate) use prefixes::compute_static_prefixes;
