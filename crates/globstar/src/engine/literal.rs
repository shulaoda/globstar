use crate::dir_match::DirMatch;

#[derive(Debug, Clone)]
pub struct LiteralMatcher {
    pub(crate) literal: Vec<u8>,
    pub(crate) case_insensitive: bool,
}

impl LiteralMatcher {
    /// Pure literal: the literal IS the prefix. Strip any trailing `/`
    /// for walker compatibility (mirrors the JS engine's staticPrefixes).
    pub fn static_prefixes(&self) -> Vec<Vec<u8>> {
        let mut bytes = self.literal.clone();
        while bytes.last() == Some(&b'/') {
            bytes.pop();
        }
        vec![bytes]
    }

    pub fn new(literal: Vec<u8>, case_insensitive: bool) -> Self {
        Self {
            literal,
            case_insensitive,
        }
    }

    pub fn is_match(&self, path: &[u8]) -> bool {
        self.path_eq(path)
    }

    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        if self.path_eq(dir_path) {
            return DirMatch::Match;
        }
        if self.literal_under(dir_path) {
            return DirMatch::Descend;
        }
        DirMatch::Pruned
    }

    fn path_eq(&self, path: &[u8]) -> bool {
        let n = self.match_prefix(path);
        n == self.literal.len() && n == path.len()
    }

    fn literal_under(&self, dir_path: &[u8]) -> bool {
        if dir_path.is_empty() {
            return true;
        }
        let n = self.match_prefix(dir_path);
        n == dir_path.len() && n < self.literal.len() && self.literal[n] == b'/'
    }

    #[inline(always)]
    fn eq_byte(&self, a: u8, b: u8) -> bool {
        if self.case_insensitive {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    }

    fn match_prefix(&self, other: &[u8]) -> usize {
        let literal = &self.literal;
        let mut n = 0usize;
        while n < literal.len() && n < other.len() {
            let lb = literal[n];
            let ob = other[n];
            if (lb == b'/' && std::path::is_separator(ob as char)) || self.eq_byte(lb, ob) {
                n += 1;
            } else {
                break;
            }
        }
        n
    }
}
