//! The engine for a pattern without wildcards: one compare against the
//! literal, separators and case read as `bytes::pat_byte_eq` does.

use crate::bytes::{ends_with_pattern, has_escaped_sep, pat_byte_eq};
use crate::dir_match::DirMatch;

#[derive(Debug, Clone)]
pub(crate) struct LiteralMatcher {
    literal: Box<[u8]>,
    case_insensitive: bool,
}

impl LiteralMatcher {
    pub(crate) fn new(literal: Box<[u8]>, case_insensitive: bool) -> Self {
        Self {
            literal,
            case_insensitive,
        }
    }

    pub(crate) fn static_prefixes(&self) -> Vec<Vec<u8>> {
        // An escaped `\` on Windows is a separator the literal never
        // matches (GLOB_SPEC §4.2): nothing to seed.
        if has_escaped_sep(&self.literal) {
            return Vec::new();
        }
        let mut bytes = &self.literal[..];
        while let [rest @ .., b'/'] = bytes {
            bytes = rest;
        }
        vec![bytes.to_vec()]
    }

    pub(crate) fn is_match(&self, path: &[u8]) -> bool {
        path.len() == self.literal.len()
            && ends_with_pattern(path, &self.literal, self.case_insensitive)
    }

    /// The directory is the literal, a whole segment run of it (the root
    /// `""` included), or neither.
    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        let n = self.prefix_len(dir_path);
        if n == dir_path.len() && n == self.literal.len() {
            DirMatch::Match
        } else if dir_path.is_empty() || (n == dir_path.len() && self.literal.get(n) == Some(&b'/'))
        {
            DirMatch::Descend
        } else {
            DirMatch::Pruned
        }
    }

    fn prefix_len(&self, other: &[u8]) -> usize {
        let literal = &self.literal;
        let mut n = 0usize;
        while n < literal.len()
            && n < other.len()
            && pat_byte_eq(literal[n], other[n], self.case_insensitive)
        {
            n += 1;
        }
        n
    }
}
