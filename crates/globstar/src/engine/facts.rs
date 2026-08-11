use crate::engine::ops::Op;

#[derive(Debug, Clone)]
pub struct LiteralFacts {
    pub(crate) suffix: Box<[u8]>,
    pub(crate) suffix_set: Box<[Box<[u8]>]>,
    pub(crate) case_insensitive: bool,
}

impl LiteralFacts {
    pub fn extract(ops: &[Op], case_insensitive: bool) -> Self {
        let suffix = extract_suffix(ops);
        let suffix_set = if suffix.is_empty() {
            extract_suffix_set(ops)
        } else {
            Vec::new()
        };
        Self {
            suffix: suffix.into_boxed_slice(),
            suffix_set: suffix_set.into_boxed_slice(),
            case_insensitive,
        }
    }

    #[inline(always)]
    pub fn accept(&self, path: &[u8]) -> bool {
        if !self.suffix.is_empty() {
            return self.ends_with(path, &self.suffix);
        }
        if !self.suffix_set.is_empty() {
            return self.suffix_set.iter().any(|s| self.ends_with(path, s));
        }
        true
    }

    fn ends_with(&self, path: &[u8], suffix: &[u8]) -> bool {
        let mut suffix_i = suffix.len();
        let mut path_i = path.len();
        while suffix_i > 0 {
            if path_i == 0 {
                return false;
            }
            suffix_i -= 1;
            path_i -= 1;
            let sb = suffix[suffix_i];
            let pb = path[path_i];
            if sb == b'/' {
                if !std::path::is_separator(pb as char) {
                    return false;
                }
                continue;
            }
            let is_equal = if self.case_insensitive {
                sb.eq_ignore_ascii_case(&pb)
            } else {
                sb == pb
            };
            if !is_equal {
                return false;
            }
        }
        true
    }
}

fn extract_suffix(ops: &[Op]) -> Vec<u8> {
    let start = ops
        .iter()
        .rposition(|op| !matches!(op, Op::Lit(_) | Op::Sep))
        .map_or(0, |i| i + 1);
    let mut acc = Vec::new();
    for op in &ops[start..] {
        match op {
            Op::Lit(bytes) => acc.extend_from_slice(bytes),
            _ => acc.push(b'/'),
        }
    }
    acc
}

fn extract_suffix_set(ops: &[Op]) -> Vec<Box<[u8]>> {
    let alt_branches = match ops.last() {
        Some(Op::Alternation(branches)) => branches,
        _ => return Vec::new(),
    };

    let pre_alt = &ops[..ops.len() - 1];
    let common_tail = extract_suffix(pre_alt);

    let mut set = Vec::with_capacity(alt_branches.len());
    for branch in alt_branches {
        let branch_suffix = extract_suffix(branch);
        if branch_suffix.is_empty() && !branch.is_empty() {
            return Vec::new();
        }
        let branch_all_literal = branch.iter().all(|op| matches!(op, Op::Lit(_) | Op::Sep));
        let full = if branch_all_literal {
            let mut v = Vec::with_capacity(common_tail.len() + branch_suffix.len());
            v.extend_from_slice(&common_tail);
            v.extend_from_slice(&branch_suffix);
            v
        } else {
            branch_suffix
        };
        if full.is_empty() {
            return Vec::new();
        }
        set.push(full.into_boxed_slice());
    }
    set
}
