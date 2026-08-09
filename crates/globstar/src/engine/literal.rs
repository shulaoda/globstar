use crate::dir_match::DirMatch;

#[derive(Debug, Clone)]
pub struct LiteralMatcher {
    pub(crate) literal: Vec<u8>,
    pub(crate) case_insensitive: bool,
}

impl LiteralMatcher {
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
        let (lit_i, path_i) = self.match_prefix(path);
        lit_i == self.literal.len() && path_i == path.len()
    }

    fn literal_under(&self, dir_path: &[u8]) -> bool {
        if dir_path.is_empty() {
            return true;
        }
        let (lit_i, dir_i) = self.match_prefix(dir_path);
        dir_i == dir_path.len() && lit_i < self.literal.len() && self.literal[lit_i] == b'/'
    }

    #[inline(always)]
    fn eq_byte(&self, a: u8, b: u8) -> bool {
        if self.case_insensitive {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    }

    fn match_prefix(&self, other: &[u8]) -> (usize, usize) {
        let literal = &self.literal;
        let mut lit_i = 0usize;
        let mut oth_i = 0usize;
        while lit_i < literal.len() && oth_i < other.len() {
            let lb = literal[lit_i];
            let ob = other[oth_i];
            if (lb == b'/' && std::path::is_separator(ob as char)) || self.eq_byte(lb, ob) {
                lit_i += 1;
                oth_i += 1;
            } else {
                break;
            }
        }
        (lit_i, oth_i)
    }
}
