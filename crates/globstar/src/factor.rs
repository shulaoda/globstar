//! AST-level factoring for [`crate::Glob::union`] brace branches.
//!
//! Without factoring, `union(["**/*.ts", "**/*.tsx", ...])` produces a
//! brace where every branch carries a duplicated `**/*` prefix; the DFA
//! grows linearly with N. Lifting common leading + trailing fragments
//! out makes `union(["**/*.ts","**/*.tsx"])` equivalent to the
//! hand-written `**/*.{ts,tsx}` (same DFA, same cost).
//!
//! Two phases per side (lifting from the front, then mirrored from the back):
//!
//! 1. **Atomic fold-group lift** — singletons (`Sep` / `Globstar` /
//!    `AnyChar` / `Star`) and structurally-equal `Literal`s. `Globstar`
//!    + flanking `Sep` are lifted as one atomic group so the lowering
//!      fold (`Globstar Sep` → `OptSegmentsSlash`, `Sep Globstar` →
//!      `SlashAnything`) is preserved.
//! 2. **Byte-level lift** on the next/last `Literal` when all branches
//!    share an opening/closing byte run. `Literal`s never participate
//!    in folds, so this is always safe.

use crate::ast::Node;

/// Take a list of brace branches and return a single `Node` with shared
/// prefix/suffix lifted out and the residuals re-wrapped in a fresh
/// brace (or returned bare if exactly one residual remains).
pub fn factor_branches(branches: Vec<Node>) -> Node {
    let mut seqs: Vec<Vec<Node>> = branches.into_iter().map(into_seq).collect();
    let prefix = lift_prefix(&mut seqs);
    let suffix = lift_suffix(&mut seqs);

    let inner = match seqs.len() {
        1 => from_seq(seqs.into_iter().next().unwrap()),
        _ => Node::Brace(seqs.into_iter().map(from_seq).collect()),
    };

    if prefix.is_empty() && suffix.is_empty() {
        return inner;
    }
    let mut out = Vec::with_capacity(prefix.len() + 1 + suffix.len());
    out.extend(prefix);
    match inner {
        Node::Concat(xs) => out.extend(xs),
        other => out.push(other),
    }
    out.extend(suffix);
    if out.len() == 1 {
        out.into_iter().next().unwrap()
    } else {
        Node::Concat(out)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────

fn into_seq(node: Node) -> Vec<Node> {
    match node {
        Node::Concat(xs) => xs,
        other => vec![other],
    }
}

fn from_seq(mut seq: Vec<Node>) -> Node {
    match seq.len() {
        0 => Node::Concat(Vec::new()), // epsilon branch
        1 => seq.pop().unwrap(),
        _ => Node::Concat(seq),
    }
}

/// Size of the fold group anchored at `seq[i]` looking forward. Mirrors
/// the `fold_globstars` passes in `engine::ops`. Lifting a partial group
/// would change the lowered semantics, so the lift loops below only
/// consume whole groups.
///
/// - `Globstar [Sep]`     → 2 (or 1 if no trailing Sep)
/// - `Sep Globstar [Sep]` → 2 or 3 (matches `/**` or mid-pattern `/**/`)
/// - anything else        → 1 (atomic)
fn fold_group_at_start(seq: &[Node], i: usize) -> usize {
    let Some(a) = seq.get(i) else { return 0 };
    match a {
        Node::Globstar => match seq.get(i + 1) {
            Some(Node::Separator) => 2,
            _ => 1,
        },
        Node::Separator if matches!(seq.get(i + 1), Some(Node::Globstar)) => match seq.get(i + 2) {
            Some(Node::Separator) => 3,
            _ => 2,
        },
        _ => 1,
    }
}

/// Mirror of [`fold_group_at_start`] for the trailing edge.
fn fold_group_at_end(seq: &[Node]) -> usize {
    let n = seq.len();
    if n == 0 {
        return 0;
    }
    match &seq[n - 1] {
        Node::Globstar if n >= 2 && matches!(seq[n - 2], Node::Separator) => 2,
        Node::Globstar => 1,
        Node::Separator if n >= 2 && matches!(seq[n - 2], Node::Globstar) => {
            if n >= 3 && matches!(seq[n - 3], Node::Separator) {
                3
            } else {
                2
            }
        }
        _ => 1,
    }
}

fn lift_prefix(seqs: &mut [Vec<Node>]) -> Vec<Node> {
    let mut lifted = Vec::new();

    // Phase 1: atomic fold groups shared across all branches. Move out
    // of `seqs[0]` (no clones); drop the same prefix from the rest.
    loop {
        let size = fold_group_at_start(&seqs[0], 0);
        if size == 0 {
            return lifted;
        }
        let head = &seqs[0][..size];
        let same = seqs
            .iter()
            .skip(1)
            .all(|s| fold_group_at_start(s, 0) == size && s[..size] == *head);
        if !same {
            break;
        }
        lifted.extend(seqs[0].drain(..size));
        for s in seqs.iter_mut().skip(1) {
            s.drain(..size);
        }
    }

    // Phase 2: byte-level Lit prefix. Lits are never fold-bound, so any
    // shared opening byte run is safe to lift.
    if !seqs
        .iter()
        .all(|s| matches!(s.first(), Some(Node::Literal(_))))
    {
        return lifted;
    }
    let n = common_byte_prefix(seqs.iter().map(|s| first_lit_bytes(s)));
    if n == 0 {
        return lifted;
    }
    // Take the lifted bytes from `seqs[0]`'s first Lit (no clone of the
    // shared run), then drain the same prefix from each branch in place.
    let Node::Literal(b0) = &mut seqs[0][0] else {
        unreachable!("checked above");
    };
    let lifted_bytes: Vec<u8> = b0.drain(..n).collect();
    if b0.is_empty() {
        seqs[0].remove(0);
    }
    for s in seqs.iter_mut().skip(1) {
        let Node::Literal(b) = &mut s[0] else {
            unreachable!("checked above");
        };
        b.drain(..n);
        if b.is_empty() {
            s.remove(0);
        }
    }
    lifted.push(Node::Literal(lifted_bytes));
    lifted
}

fn lift_suffix(seqs: &mut [Vec<Node>]) -> Vec<Node> {
    // Build outermost-first, then reverse once at the end so the caller
    // sees natural inner→outer order.
    let mut lifted_reverse = Vec::new();

    // Phase 1: atomic fold groups at the trailing edge.
    loop {
        let size = fold_group_at_end(&seqs[0]);
        if size == 0 {
            break;
        }
        let len0 = seqs[0].len();
        let tail = &seqs[0][len0 - size..];
        let same = seqs
            .iter()
            .skip(1)
            .all(|s| fold_group_at_end(s) == size && s[s.len() - size..] == *tail);
        if !same {
            break;
        }
        // Drain the trailing range in REVERSE so the elements land in
        // outermost-first order (matched by the final `reverse()`).
        lifted_reverse.extend(seqs[0].drain(len0 - size..).rev());
        for s in seqs.iter_mut().skip(1) {
            s.truncate(s.len() - size);
        }
    }

    // Phase 2: byte-level Lit suffix.
    if seqs
        .iter()
        .all(|s| matches!(s.last(), Some(Node::Literal(_))))
    {
        let n = common_byte_suffix(seqs.iter().map(|s| last_lit_bytes(s)));
        if n > 0 {
            // Same in-place strategy as the prefix phase: take the
            // lifted bytes from `seqs[0]`, truncate the rest in place.
            let last0 = seqs[0].len() - 1;
            let Node::Literal(b0) = &mut seqs[0][last0] else {
                unreachable!("checked above");
            };
            let lifted_bytes = b0.split_off(b0.len() - n);
            if b0.is_empty() {
                seqs[0].pop();
            }
            for s in seqs.iter_mut().skip(1) {
                let last = s.len() - 1;
                let Node::Literal(b) = &mut s[last] else {
                    unreachable!("checked above");
                };
                b.truncate(b.len() - n);
                if b.is_empty() {
                    s.pop();
                }
            }
            lifted_reverse.push(Node::Literal(lifted_bytes));
        }
    }

    lifted_reverse.reverse();
    lifted_reverse
}

// ── Byte-prefix / byte-suffix helpers ───────────────────────────────────

fn first_lit_bytes(seq: &[Node]) -> &[u8] {
    match &seq[0] {
        Node::Literal(b) => b.as_slice(),
        _ => unreachable!("caller ensures seq starts with a Literal"),
    }
}

fn last_lit_bytes(seq: &[Node]) -> &[u8] {
    match seq.last().unwrap() {
        Node::Literal(b) => b.as_slice(),
        _ => unreachable!("caller ensures seq ends with a Literal"),
    }
}

fn common_byte_prefix<'a, I: IntoIterator<Item = &'a [u8]>>(iter: I) -> usize {
    let lits: Vec<&[u8]> = iter.into_iter().collect();
    let min = lits.iter().map(|l| l.len()).min().unwrap_or(0);
    (0..min)
        .take_while(|&i| {
            let b = lits[0][i];
            lits.iter().skip(1).all(|l| l[i] == b)
        })
        .count()
}

fn common_byte_suffix<'a, I: IntoIterator<Item = &'a [u8]>>(iter: I) -> usize {
    let lits: Vec<&[u8]> = iter.into_iter().collect();
    let min = lits.iter().map(|l| l.len()).min().unwrap_or(0);
    (0..min)
        .take_while(|&i| {
            let b = lits[0][lits[0].len() - 1 - i];
            lits.iter().skip(1).all(|l| l[l.len() - 1 - i] == b)
        })
        .count()
}
