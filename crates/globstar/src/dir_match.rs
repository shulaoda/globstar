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
}

impl DirMatch {
    #[inline]
    pub fn is_match(self) -> bool {
        matches!(self, Self::Match | Self::DescendAndMatch)
    }

    #[inline]
    pub fn should_descend(self) -> bool {
        matches!(self, Self::Descend | Self::DescendAndMatch)
    }

    #[inline]
    pub fn is_pruned(self) -> bool {
        matches!(self, Self::Pruned)
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
}
