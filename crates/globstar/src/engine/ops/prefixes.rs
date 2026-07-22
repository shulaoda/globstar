//! Static path-prefix analysis used by filesystem walkers.

use std::collections::BTreeSet;

use super::ir::Op;

pub fn extract_prefix(ops: &[Op]) -> Vec<u8> {
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
    acc
}

pub fn compute_static_prefixes(ops: &[Op]) -> Box<[Box<[u8]>]> {
    dedupe_prefixes(extract_prefixes_per_branch(ops))
        .into_iter()
        .map(Vec::into_boxed_slice)
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn extract_prefixes_per_branch(ops: &[Op]) -> Vec<Vec<u8>> {
    match ops.first() {
        Some(Op::Alternation(branches)) => branches
            .iter()
            .flat_map(|branch| extract_prefixes_per_branch(branch))
            .collect(),
        _ => vec![extract_prefix(ops)],
    }
}

pub(crate) fn dedupe_prefixes(mut prefixes: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    // Ancestors are considered before descendants. The ordered set owns each
    // accepted buffer once and supports borrowed-slice lookups at every `/`
    // boundary, avoiding the old all-accepted × all-candidate scan.
    prefixes.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let mut accepted: BTreeSet<Vec<u8>> = BTreeSet::new();
    for prefix in prefixes {
        let duplicate = accepted.contains(prefix.as_slice());
        let covered = accepted.contains(&[][..])
            || prefix
                .iter()
                .enumerate()
                .any(|(i, &b)| b == b'/' && accepted.contains(&prefix[..i]));
        if !duplicate && !covered {
            accepted.insert(prefix);
        }
    }
    let mut result: Vec<_> = accepted.into_iter().collect();
    result.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    result
}

#[cfg(test)]
mod tests {
    use super::dedupe_prefixes;

    #[test]
    fn dedupe_respects_directory_boundaries() {
        let got = dedupe_prefixes(
            ["src", "src/cli", "src-other", "src2", "src"]
                .into_iter()
                .map(|s| s.as_bytes().to_vec())
                .collect(),
        );
        assert_eq!(
            got,
            ["src", "src2", "src-other"]
                .into_iter()
                .map(|s| s.as_bytes().to_vec())
                .collect::<Vec<_>>()
        );
    }
}
