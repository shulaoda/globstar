use std::collections::BTreeSet;

use super::ir::Op;

pub fn compute_static_prefixes(ops: &[Op]) -> Box<[Box<[u8]>]> {
    dedupe_prefixes(extract_prefixes_per_branch(ops))
}

fn extract_prefix(ops: &[Op]) -> Box<[u8]> {
    let mut acc = Vec::new();
    let mut last_boundary = 0usize;
    let mut fully_literal = true;
    for op in ops {
        match op {
            Op::Lit(bytes) => acc.extend_from_slice(bytes),
            Op::Sep | Op::SepRun => {
                acc.push(b'/');
                last_boundary = acc.len();
            }
            _ => {
                fully_literal = false;
                break;
            }
        }
    }
    if !fully_literal {
        acc.truncate(last_boundary);
    }
    while acc.last() == Some(&b'/') {
        acc.pop();
    }
    acc.into_boxed_slice()
}

fn extract_prefixes_per_branch(ops: &[Op]) -> Box<[Box<[u8]>]> {
    if let Some(Op::Alternation(branches)) = ops.first() {
        if matches!(ops.get(1), None | Some(Op::Sep) | Some(Op::SepRun)) {
            return branches
                .iter()
                .flat_map(|branch| extract_prefixes_per_branch(branch))
                .collect::<Box<_>>();
        }
    }
    Box::new([extract_prefix(ops)])
}

fn dedupe_prefixes(mut prefixes: Box<[Box<[u8]>]>) -> Box<[Box<[u8]>]> {
    if prefixes.len() <= 1 {
        return prefixes;
    }
    prefixes.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let mut accepted = BTreeSet::new();
    for prefix in prefixes {
        if prefix.is_empty() {
            return Box::new([prefix]);
        }
        let covered = prefix
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'/' && accepted.contains(&prefix[..i]));
        if !covered {
            accepted.insert(prefix);
        }
    }
    let mut result = accepted.into_iter().collect::<Box<_>>();
    result.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    result
}
