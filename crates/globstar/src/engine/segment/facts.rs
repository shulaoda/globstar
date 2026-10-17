//! Literal facts read off the ops at compile time: the suffix
//! prefilter and the static prefixes a walker seeds from.

use crate::engine::ops::Op;

use super::compile::{len_then_bytes, op_weight};
use super::{Bytes, has_sep};

/// Largest suffix set worth scanning before the engine runs; a
/// bigger alternation gets no prefilter (the engine indexes it).
const MAX_SUFFIX_SET: usize = 16;

/// Literal suffixes one of which every match ends with: the trailing
/// `Lit`/`Sep` run (separators as `/`), or when the pattern ends in a
/// brace, one per branch. Empty when nothing is guaranteed.
pub(super) fn suffixes(ops: &[Op]) -> Box<[Bytes]> {
    let run = trailing(ops);
    if run.is_empty() {
        return suffix_set(ops);
    }
    let mut acc = Vec::with_capacity(run.iter().map(op_weight).sum());
    push_run(run, &mut acc);
    Box::new([acc.into_boxed_slice()])
}

/// The `Lit`/`Sep` run that ends `ops`.
fn trailing(ops: &[Op]) -> &[Op] {
    let from = ops
        .iter()
        .rposition(|op| !matches!(op, Op::Lit(_) | Op::Sep))
        .map_or(0, |i| i + 1);
    &ops[from..]
}

fn push_run(run: &[Op], acc: &mut Vec<u8>) {
    for op in run {
        match op {
            Op::Lit(bytes) => acc.extend_from_slice(bytes),
            _ => acc.push(b'/'),
        }
    }
}

fn push_suffix(ops: &[Op], acc: &mut Vec<u8>) {
    push_run(trailing(ops), acc);
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

/// How deep brace expansions may nest along one static prefix, and the
/// part of its budget that does not depend on the pattern.
const MAX_HEAD_DEPTH: usize = 64;
const HEAD_BUDGET_FLOOR: usize = 4096;

/// The deepest segment-bounded literal head of each expansion,
/// deduplicated, sorted by (length, bytes). A brace in the head is taken
/// branch by branch with what follows it (`{a,b}/c` gives `a/c` and
/// `b/c`), so a factored union keeps its members' heads.
///
/// Every byte read and every branch taken costs one unit of a budget of
/// `HEAD_BUDGET_FLOOR` plus twice the pattern's weight (`op_weight`, only
/// worked out once the floor is spent): reading each op once costs
/// at most its weight, and re-reading what follows a brace for each
/// branch is what the rest pays for. Past the budget, or past
/// `MAX_HEAD_DEPTH` nested braces, the head ends at its last whole
/// segment, a shorter prefix that still holds: `{a,b}{c,d}…` stops after
/// a few braces instead of doubling, and a brace before a long literal
/// copies it a few times, not once per branch.
pub(super) fn static_prefixes(ops: &[Op]) -> Box<[Bytes]> {
    let mut heads = Heads {
        root: ops,
        spent: 0,
        limit: HEAD_BUDGET_FLOOR,
        weighed: false,
        out: Vec::with_capacity(1),
    };
    heads.read(ops, None, &mut Vec::new(), 0, 0);
    let out = if heads.out.len() > 1 {
        dedupe(heads.out)
    } else {
        heads.out
    };
    out.into_boxed_slice()
}

/// What follows the ops being read: the rest of an enclosing sequence.
struct Then<'a> {
    ops: &'a [Op],
    next: Option<&'a Then<'a>>,
}

struct Heads<'r> {
    root: &'r [Op],
    spent: usize,
    /// The floor until it is spent, then the floor plus twice the weight.
    limit: usize,
    weighed: bool,
    out: Vec<Bytes>,
}

impl Heads<'_> {
    /// Spend `n` units, unless that passes the limit.
    fn afford(&mut self, n: usize) -> bool {
        if self.spent + n > self.limit && !self.weighed {
            self.weighed = true;
            self.limit += 2 * self.root.iter().map(op_weight).sum::<usize>();
        }
        if self.spent + n > self.limit {
            return false;
        }
        self.spent += n;
        true
    }

    /// An empty head covers every other one: nothing more to read.
    fn covered(&self) -> bool {
        self.out.last().is_some_and(|h| h.is_empty())
    }

    /// Read `ops`, then `then`, after `acc` (whose last whole segment
    /// ends at `boundary`), and record the head each expansion reaches.
    fn read<'a>(
        &mut self,
        mut ops: &'a [Op],
        mut then: Option<&'a Then<'a>>,
        acc: &mut Vec<u8>,
        mut boundary: usize,
        depth: usize,
    ) {
        let mark = acc.len();
        let end = loop {
            let Some((op, rest)) = ops.split_first() else {
                match then {
                    Some(t) => {
                        ops = t.ops;
                        then = t.next;
                        continue;
                    }
                    None => break acc.len(), // all literal
                }
            };
            match op {
                // An escaped `\` on Windows: the expansion matches nothing
                // (§2.4), so it has no head to seed.
                Op::Lit(bytes) if has_sep(bytes) => {
                    acc.truncate(mark);
                    return;
                }
                Op::Lit(bytes) if self.afford(bytes.len()) => acc.extend_from_slice(bytes),
                Op::Sep if self.afford(1) => {
                    acc.push(b'/');
                    boundary = acc.len();
                }
                Op::Alternation(bs) if depth < MAX_HEAD_DEPTH && self.afford(bs.len()) => {
                    let after = Then {
                        ops: rest,
                        next: then,
                    };
                    for branch in bs {
                        if self.covered() {
                            break;
                        }
                        self.read(branch, Some(&after), acc, boundary, depth + 1);
                    }
                    acc.truncate(mark);
                    return;
                }
                // Every branch opening a new segment leaves what came
                // before whole.
                Op::Alternation(bs) if bs.iter().all(|b| matches!(b.first(), Some(Op::Sep))) => {
                    break acc.len();
                }
                _ => break boundary,
            }
            ops = rest;
        };
        let mut head = &acc[..end];
        while let [rest @ .., b'/'] = head {
            head = rest;
        }
        self.out.push(Bytes::from(head));
        acc.truncate(mark);
    }
}

/// Drop prefixes a shorter one already covers; an empty one covers all.
/// Shortest first, so a prefix is only ever covered by one kept before.
fn dedupe(mut prefixes: Vec<Bytes>) -> Vec<Bytes> {
    prefixes.sort_unstable_by(len_then_bytes);
    prefixes.dedup();
    let mut kept: Vec<Bytes> = Vec::new();
    for p in prefixes {
        if p.is_empty() {
            return vec![p];
        }
        let covered = kept
            .iter()
            .any(|k| p.len() > k.len() && p[k.len()] == b'/' && p[..k.len()] == k[..]);
        if !covered {
            kept.push(p);
        }
    }
    kept
}
