#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirMatch {
    /// The directory matches; nothing below it can.
    Match,
    /// Neither the directory nor anything below it matches.
    Pruned,
    /// The directory does not match; something below it may.
    Descend,
    /// The directory matches, and something below it may.
    DescendAndMatch,
    /// The directory does not match; everything below it does.
    DescendAll,
    /// The directory matches, and so does everything below it.
    DescendAllAndMatch,
}

impl DirMatch {
    pub fn new(matches: bool, may_below: bool, all_below: bool) -> Self {
        match (matches, may_below || all_below, all_below) {
            (true, false, _) => Self::Match,
            (false, false, _) => Self::Pruned,
            (false, true, false) => Self::Descend,
            (false, true, true) => Self::DescendAll,
            (true, true, false) => Self::DescendAndMatch,
            (true, true, true) => Self::DescendAllAndMatch,
        }
    }

    #[inline]
    pub fn matches(self) -> bool {
        matches!(
            self,
            Self::Match | Self::DescendAndMatch | Self::DescendAllAndMatch
        )
    }

    #[inline]
    pub fn may_match_below(self) -> bool {
        !matches!(self, Self::Match | Self::Pruned)
    }

    #[inline]
    pub fn matches_all_below(self) -> bool {
        matches!(self, Self::DescendAll | Self::DescendAllAndMatch)
    }
}
