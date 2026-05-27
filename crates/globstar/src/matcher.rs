//! The [`Matcher`] trait — the walker's interface to a compiled pattern.
//!
//! Implemented by [`crate::Glob`] (single pattern, including the
//! brace-merged form returned by [`crate::Glob::union`]). The trait
//! lets [`globstar_walk::Walk`](../../globstar_walk/struct.Walk.html)
//! and other consumers be generic over the matcher type with zero
//! dispatch overhead (static dispatch via monomorphization).
//!
//! The trait is deliberately narrow: just the three operations a walker
//! needs. Extending it is a breaking change, so keep it minimal.

use crate::DirMatch;

/// The minimal interface a walker needs from a compiled pattern.
///
/// Object-safe: `&dyn Matcher` works, if a caller prefers dynamic dispatch
/// over monomorphization.
pub trait Matcher {
    /// Whether `path` is a full match.
    fn is_match(&self, path: &[u8]) -> bool;

    /// Four-way directory query for walker pruning. See [`DirMatch`] for
    /// semantics.
    fn match_dir(&self, dir_path: &[u8]) -> DirMatch;

    /// Static byte prefixes at which walker traversal can start. Each
    /// entry is a segment-bounded relative path (possibly empty). The
    /// walker resolves each against its root directory.
    fn static_prefixes(&self) -> Vec<Vec<u8>>;
}
