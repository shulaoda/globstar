//! The [`Matcher`] trait — the interface a walker needs from a compiled
//! pattern, implemented by [`crate::Glob`]. Deliberately narrow (just the
//! three operations a walker uses); extending it is a breaking change, so
//! keep it minimal.

use crate::DirMatch;

/// The minimal interface a walker needs from a compiled pattern.
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
