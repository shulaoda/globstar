//! Literal facts read off the ops at compile time: the suffix
//! prefilter and the static prefixes a walker seeds from.

use std::collections::BTreeSet;

use crate::engine::ops::Op;

use super::Bytes;

/// Largest suffix set worth scanning before the engine runs; a
/// bigger alternation gets no prefilter (the engine indexes it).
const MAX_SUFFIX_SET: usize = 16;

/// Literal suffixes one of which every match ends with: the trailing
/// `Lit`/`Sep` run (separators as `/`), or when the pattern ends in a
/// brace, one per branch. Empty when nothing is guaranteed.
pub(super) fn suffixes(ops: &[Op]) -> Box<[Bytes]> {
    let mut acc = Vec::new();
    push_suffix(ops, &mut acc);
    if !acc.is_empty() {
        return Box::new([acc.into_boxed_slice()]);
    }
    suffix_set(ops)
}

fn push_suffix(ops: &[Op], acc: &mut Vec<u8>) {
    let from = ops
        .iter()
        .rposition(|op| !matches!(op, Op::Lit(_) | Op::Sep))
        .map_or(0, |i| i + 1);
    for op in &ops[from..] {
        match op {
            Op::Lit(bytes) => acc.extend_from_slice(bytes),
            _ => acc.push(b'/'),
        }
    }
}

/// When the pattern ends in a brace: one guaranteed suffix per branch,
/// or none when a branch guarantees nothing.
fn suffix_set(ops: &[Op]) -> Box<[Bytes]> {
    let branches = match ops.last() {
        Some(Op::Alternation(branches)) if branches.len() <= MAX_SUFFIX_SET => branches,
        _ => return Box::default(),
    };
    let mut common = Vec::new();
    push_suffix(&ops[..ops.len() - 1], &mut common);
    let mut set = Vec::with_capacity(branches.len());
    let mut acc = Vec::new();
    for branch in branches {
        acc.clear();
        // Gluing the common tail to a branch that is not all-literal
        // would claim more than the branch guarantees.
        if branch.iter().all(|op| matches!(op, Op::Lit(_) | Op::Sep)) {
            acc.extend_from_slice(&common);
        }
        push_suffix(branch, &mut acc);
        if acc.is_empty() {
            return Box::default();
        }
        set.push(Bytes::from(&acc[..]));
    }
    set.into_boxed_slice()
}

/// The deepest segment-bounded literal prefix per top-level brace
/// branch, deduplicated, sorted by (length, bytes).
pub(super) fn static_prefixes(ops: &[Op]) -> Box<[Bytes]> {
    let mut out = Vec::with_capacity(1);
    let mut acc = Vec::new();
    prefixes_per_branch(ops, &mut acc, &mut out);
    if out.len() > 1 {
        out = dedupe(out);
    }
    out.into_boxed_slice()
}

fn prefixes_per_branch(ops: &[Op], acc: &mut Vec<u8>, out: &mut Vec<Bytes>) {
    if let Some(Op::Alternation(branches)) = ops.first() {
        if matches!(
            ops.get(1),
            None | Some(Op::Sep) | Some(Op::SepRun) | Some(Op::SlashAnything)
        ) {
            for branch in branches {
                prefixes_per_branch(branch, acc, out);
            }
            return;
        }
    }
    acc.clear();
    let mut last_boundary = 0usize;
    let mut fully_literal = true;
    for op in ops {
        match op {
            Op::Lit(bytes) => acc.extend_from_slice(bytes),
            Op::Sep | Op::SepRun => {
                acc.push(b'/');
                last_boundary = acc.len();
            }
            Op::SlashAnything => {
                last_boundary = acc.len();
                fully_literal = false;
                break;
            }
            // Every branch opens a new segment, so what came before is whole.
            Op::Alternation(branches)
                if branches
                    .iter()
                    .all(|b| matches!(b.first(), Some(Op::Sep | Op::SepRun))) =>
            {
                last_boundary = acc.len();
                fully_literal = false;
                break;
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
    out.push(Bytes::from(&acc[..]));
}

/// Drop prefixes a shorter one already covers; an empty one covers all.
fn dedupe(mut prefixes: Vec<Bytes>) -> Vec<Bytes> {
    prefixes.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a[..].cmp(&b[..])));
    let mut accepted: BTreeSet<Vec<u8>> = BTreeSet::new();
    for p in prefixes {
        if p.is_empty() {
            return vec![p];
        }
        let covered = p
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'/' && accepted.contains(&p[..i]));
        if !covered {
            accepted.insert(p.to_vec());
        }
    }
    let mut result: Vec<Bytes> = accepted.into_iter().map(Vec::into_boxed_slice).collect();
    result.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a[..].cmp(&b[..])));
    result
}
