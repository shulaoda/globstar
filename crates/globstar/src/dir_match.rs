//! `match_dir` result type. See `theory/08-walker-and-pruning.md` and ADR-005.

/// Result of querying whether a directory path can possibly contain matches.
///
/// Returned by [`crate::Glob::match_dir`]. Walkers use this to prune
/// entire subtrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMatch {
    /// No string with this directory as prefix can match. Walker should prune
    /// the entire subtree.
    Pruned,
    /// The directory itself does not match, but some descendant might. Walker
    /// should descend.
    Descend,
    /// The directory itself matches, and no descendant can match further.
    /// Walker may yield it.
    Match,
    /// The directory itself matches, AND some descendant might also match.
    /// Walker should yield it AND descend.
    DescendAndMatch,
}

impl DirMatch {
    /// Whether the directory should be yielded as a match.
    pub fn is_match(self) -> bool {
        matches!(self, Self::Match | Self::DescendAndMatch)
    }

    /// Whether the walker should descend into this directory.
    pub fn should_descend(self) -> bool {
        matches!(self, Self::Descend | Self::DescendAndMatch)
    }

    /// Whether the entire subtree can be pruned.
    pub fn is_pruned(self) -> bool {
        matches!(self, Self::Pruned)
    }

    /// Combine the two independent `match_dir` questions into a
    /// [`DirMatch`]:
    /// - `exact`: does the dir path match the pattern as-is?
    /// - `prefix`: could some `<dir>/<name>/...` descendant match?
    ///
    /// Every engine's `match_dir` answers these two booleans against
    /// its own automaton; this helper is the single place that
    /// produces the four-way enum, so the mapping stays consistent.
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
