//! The four-way result of [`Glob::match_dir`](crate::Glob::match_dir),
//! which lets a walker prune whole subtrees it never has to enter.

/// What a directory path means for the pattern: whether it matches,
/// could contain a match deeper down, both, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMatch {
    /// The directory matches, and nothing below it can. Yield it, don't descend.
    Match,

    /// Nothing at or below the directory can match. Prune the whole subtree.
    Pruned,

    /// The directory doesn't match, but a descendant might. Descend into it.
    Descend,

    /// The directory matches, and a descendant might too. Yield it and descend.
    DescendAndMatch,
}

impl DirMatch {
    /// Whether the directory itself should be yielded as a match.
    #[inline]
    pub fn is_match(self) -> bool {
        matches!(self, Self::Match | Self::DescendAndMatch)
    }

    /// Whether the walker should descend into the directory.
    #[inline]
    pub fn should_descend(self) -> bool {
        matches!(self, Self::Descend | Self::DescendAndMatch)
    }

    /// Whether the whole subtree can be skipped.
    #[inline]
    pub fn is_pruned(self) -> bool {
        matches!(self, Self::Pruned)
    }

    /// Assemble a [`DirMatch`] from the two questions each engine's
    /// `match_dir` answers about a directory `d`:
    ///
    /// - `exact`  — does `d` itself match the pattern?
    /// - `prefix` — could something below `d` match?
    ///
    /// e.g. for `src/**/*.rs`, dir `src` is `(exact: false, prefix: true)`
    /// → `Descend`; dir `src/a.rs` is `(true, false)` → `Match`.
    #[inline]
    pub fn from_exact_prefix(exact: bool, prefix: bool) -> Self {
        match (exact, prefix) {
            (true, true) => Self::DescendAndMatch,
            (true, false) => Self::Match,
            (false, true) => Self::Descend,
            (false, false) => Self::Pruned,
        }
    }
}
