use crate::dir_match::DirMatch;

use super::pat_byte_eq;

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
        // An escaped `\` on Windows is a separator the literal can never
        // match (§4.2): nothing to seed.
        if cfg!(windows)
            && self
                .literal
                .iter()
                .any(|&b| b != b'/' && std::path::is_separator(char::from(b)))
        {
            return Vec::new();
        }
        let mut bytes = &self.literal[..];
        while let [rest @ .., b'/'] = bytes {
            bytes = rest;
        }
        vec![bytes.to_vec()]
    }

    pub(crate) fn is_match(&self, path: &[u8]) -> bool {
        let n = self.match_prefix(path);
        n == self.literal.len() && n == path.len()
    }

    /// One scan: the directory is the literal itself, or a whole
    /// segment run of it (the root `""` included), or neither.
    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        let n = self.match_prefix(dir_path);
        if n == dir_path.len() && n == self.literal.len() {
            DirMatch::Match
        } else if dir_path.is_empty() || (n == dir_path.len() && self.literal.get(n) == Some(&b'/'))
        {
            DirMatch::Descend
        } else {
            DirMatch::Pruned
        }
    }

    fn match_prefix(&self, other: &[u8]) -> usize {
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
