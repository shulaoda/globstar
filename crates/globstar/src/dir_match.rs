#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMatch {
    /// The directory matches, and nothing below it can.
    Match,
    /// Nothing at or below the directory can match.
    Pruned,
    /// The directory doesn't match, but a descendant might.
    Descend,
    /// The directory matches, and a descendant might too.
    DescendAndMatch,
    /// The directory doesn't match, but everything below it does.
    DescendAll,
    /// The directory matches, and so does everything below it.
    DescendAllAndMatch,
}

impl DirMatch {
    #[inline]
    pub fn is_match(self) -> bool {
        matches!(
            self,
            Self::Match | Self::DescendAndMatch | Self::DescendAllAndMatch
        )
    }

    #[inline]
    pub fn should_descend(self) -> bool {
        !matches!(self, Self::Match | Self::Pruned)
    }

    #[inline]
    pub fn is_pruned(self) -> bool {
        matches!(self, Self::Pruned)
    }

    /// Every path below the directory matches: `is_match(dir + "/" + s)`
    /// holds for every `s` of one or more non-empty segments. `true` is
    /// a guarantee; `false` means "not all, or not provable".
    #[inline]
    pub fn matches_all_below(self) -> bool {
        matches!(self, Self::DescendAll | Self::DescendAllAndMatch)
    }

    #[inline]
    pub fn from_exact_prefix(exact: bool, prefix: bool) -> Self {
        match (exact, prefix) {
            (true, true) => Self::DescendAndMatch,
            (true, false) => Self::Match,
            (false, true) => Self::Descend,
            (false, false) => Self::Pruned,
        }
    }

    /// [`from_exact_prefix`](Self::from_exact_prefix) plus the
    /// all-below axis, which implies `prefix`.
    #[inline]
    pub fn from_exact_prefix_all(exact: bool, prefix: bool, all: bool) -> Self {
        match (exact, all) {
            (true, true) => Self::DescendAllAndMatch,
            (false, true) => Self::DescendAll,
            _ => Self::from_exact_prefix(exact, prefix),
        }
    }
}
