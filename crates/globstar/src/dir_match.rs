/// What a pattern says about a directory: whether the directory's own
/// path matches, and what can match below it.
///
/// The names read from the pattern's side: the pattern *matches* the
/// directory, *descends* below it (something below may match), or is
/// *pruned* there (nothing at or below can match). What a walker does
/// with that depends on the role the pattern plays, so the walker reads
/// the three facts, not the names:
///
/// - A pattern that *selects* files is worth walking into a directory
///   while [`may_match_below`](Self::may_match_below) holds, and when
///   [`matches_all_below`](Self::matches_all_below) holds every file
///   below is a result without another test.
/// - A pattern that *excludes* files lets the walker skip a directory
///   whole once `matches_all_below` holds, and needs no more tests below
///   a directory once `may_match_below` is `false`.
/// - Whether the directory's own match ([`matches`](Self::matches))
///   counts is the walker's call: it does for one that yields
///   directories, or that takes a matched directory as excluded whole.
///
/// `matches_all_below` is a guarantee; `may_match_below` errs toward
/// `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
    /// The verdict with these three facts; `all_below` implies
    /// `may_below`. For a consumer that combines verdicts of its own
    /// (JS: `DirMatch.from`).
    pub fn new(matches: bool, may_below: bool, all_below: bool) -> Self {
        match (matches, may_below || all_below, all_below) {
            (true, false, _) => Self::Match,
            (false, false, _) => Self::Pruned,
            (false, true, false) => Self::Descend,
            (true, true, false) => Self::DescendAndMatch,
            (false, true, true) => Self::DescendAll,
            (true, true, true) => Self::DescendAllAndMatch,
        }
    }

    /// Does the directory's own path match?
    #[inline]
    pub fn matches(self) -> bool {
        matches!(
            self,
            Self::Match | Self::DescendAndMatch | Self::DescendAllAndMatch
        )
    }

    /// May some path below the directory match? `false` is a guarantee
    /// that none does.
    #[inline]
    pub fn may_match_below(self) -> bool {
        !matches!(self, Self::Match | Self::Pruned)
    }

    /// Does every path below the directory match: `is_match(dir + "/" +
    /// s)` (just `s` when `dir` is the root `""`) for every `s` of one or
    /// more non-empty segments? `true` is a guarantee; `false` means "not
    /// all, or not provable".
    #[inline]
    pub fn matches_all_below(self) -> bool {
        matches!(self, Self::DescendAll | Self::DescendAllAndMatch)
    }
}

#[cfg(test)]
mod tests {
    use super::DirMatch::{self, *};

    const ALL: [DirMatch; 6] = [
        Match,
        Pruned,
        Descend,
        DescendAndMatch,
        DescendAll,
        DescendAllAndMatch,
    ];

    #[test]
    fn the_three_facts_name_the_verdict() {
        for v in ALL {
            assert_eq!(
                DirMatch::new(v.matches(), v.may_match_below(), v.matches_all_below()),
                v
            );
        }
    }

    #[test]
    fn all_below_implies_may_below() {
        for v in ALL {
            assert!(!v.matches_all_below() || v.may_match_below(), "{v:?}");
        }
        assert_eq!(DirMatch::new(false, false, true), DescendAll);
        assert_eq!(DirMatch::new(true, false, true), DescendAllAndMatch);
    }

    #[test]
    fn the_two_roles_read_different_facts() {
        // Selecting files: walk in unless nothing below can match.
        assert!(!Pruned.may_match_below());
        assert!(!Match.may_match_below());
        assert!(Descend.may_match_below());
        // Excluding files: skip whole only when everything below is.
        assert!(!Descend.matches_all_below());
        assert!(!DescendAndMatch.matches_all_below());
        assert!(DescendAll.matches_all_below() && !DescendAll.matches());
        assert!(DescendAllAndMatch.matches_all_below() && DescendAllAndMatch.matches());
    }
}
