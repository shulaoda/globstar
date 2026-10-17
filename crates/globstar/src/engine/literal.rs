use crate::dir_match::DirMatch;

#[derive(Debug, Clone)]
pub struct LiteralMatcher {
    literal: Box<[u8]>,
    case_insensitive: bool,
}

impl LiteralMatcher {
    pub fn new(literal: Box<[u8]>, case_insensitive: bool) -> Self {
        Self {
            literal,
            case_insensitive,
        }
    }

    pub fn static_prefixes(&self) -> Vec<Vec<u8>> {
        let mut bytes = &self.literal[..];
        while let [rest @ .., b'/'] = bytes {
            bytes = rest;
        }
        vec![bytes.to_vec()]
    }

    pub fn is_match(&self, path: &[u8]) -> bool {
        let n = self.match_prefix(path);
        n == self.literal.len() && n == path.len()
    }

    /// One scan: the directory is the literal itself, or a whole
    /// segment run of it (the root `""` included), or neither.
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
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
            // Separators only match the structural `/`; a literal `\`
            // byte (Windows separator) never does (§2.2).
            let ok = if lb == b'/' {
                std::path::is_separator(ob as char)
            } else {
                !std::path::is_separator(ob as char) && self.eq_byte(lb, ob)
            };
            if ok {
                n += 1;
            } else {
                break;
            }
        }
        n
    }
}
