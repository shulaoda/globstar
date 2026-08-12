//! AST-level factoring for [`crate::Glob::union`] brace branches.
//!
//! Without factoring, `union(["**/*.ts", "**/*.tsx", ...])` produces a
//! brace where every branch carries a duplicated `**/*` prefix; the
//! compiled program grows linearly with N. Lifting common leading + trailing fragments
//! out makes `union(["**/*.ts","**/*.tsx"])` equivalent to the
//! hand-written `**/*.{ts,tsx}` (one shared segment-program path).
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
    let mut seqs: Vec<Vec<Node>> = branches
        .into_iter()
        .map(|n| match n {
            Node::Concat(xs) => xs,
            other => vec![other],
        })
        .collect();
    let prefix = lift_prefix(&mut seqs);
    let suffix = lift_suffix(&mut seqs);

    let inner = match seqs.len() {
        1 => from_seq(seqs.into_iter().next().unwrap()),
        _ => Node::Brace(seqs.into_iter().map(from_seq).collect()),
    };

    let mut out = prefix;
    match inner {
        Node::Concat(xs) => out.extend(xs),
        other => out.push(other),
    }
    out.extend(suffix);
    if out.len() == 1 {
        out.pop().unwrap()
    } else {
        Node::Concat(out)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────

/// Structural equality for the node kinds we lift. `Literal`s compare
/// byte-for-byte; the valueless singletons (`Separator` / `Globstar` /
/// `AnyChar` / `Star`) compare by kind. `Class` / `Brace` / `Concat`
/// deliberately compare as unequal so they are never lifted.
///
/// Lifting a whole `Brace` out (which a derived `PartialEq` would do)
/// tears it from a flanking `/` that `distribute_seps` must keep next to
/// a globstar-edged branch, so `union` would stop equalling the OR of
/// its members (e.g. `union(["{**,a}/**", "{**,a}/"])`).
fn node_eq(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::Literal(x), Node::Literal(y)) => x == y,
        (Node::Separator, Node::Separator)
        | (Node::Globstar, Node::Globstar)
        | (Node::AnyChar, Node::AnyChar)
        | (Node::Star, Node::Star) => true,
        _ => false,
    }
}

fn from_seq(mut seq: Vec<Node>) -> Node {
    match seq.len() {
        0 => Node::Concat(Vec::new()), // epsilon branch
        1 => seq.pop().unwrap(),
        _ => Node::Concat(seq),
    }
}

/// Size of the fold group at one edge of a branch, read inward from the
/// edge (`lift_suffix` feeds the nodes reversed; the group shapes are
/// symmetric, so one table serves both sides). Mirrors the
/// `fold_globstars` passes in `engine::ops` — lifting a partial group
/// would change the lowered semantics, so the lift loops below only
/// consume whole groups.
///
/// - `Globstar [Sep]`     → 2 (or 1 with no adjacent Sep)
/// - `Sep Globstar [Sep]` → 2 or 3 (`/**` or mid-pattern `/**/`)
/// - empty → 0; anything else → 1 (atomic)
fn fold_group_at_edge<'a>(mut it: impl Iterator<Item = &'a Node>) -> usize {
    match (it.next(), it.next()) {
        (None, _) => 0,
        (Some(Node::Globstar), Some(Node::Separator)) => 2,
        (Some(Node::Separator), Some(Node::Globstar)) => {
            if matches!(it.next(), Some(Node::Separator)) {
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
        let size = fold_group_at_edge(seqs[0].iter());
        if size == 0 {
            return lifted;
        }
        let head = &seqs[0][..size];
        let same = seqs.iter().skip(1).all(|s| {
            fold_group_at_edge(s.iter()) == size
                && s[..size].iter().zip(head).all(|(x, y)| node_eq(x, y))
        });
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
    let lits: Vec<&[u8]> = seqs
        .iter()
        .map(|s| match &s[0] {
            Node::Literal(b) => b.as_slice(),
            _ => unreachable!("checked above"),
        })
        .collect();
    let min = lits.iter().map(|l| l.len()).min().unwrap_or(0);
    let n = (0..min)
        .take_while(|&i| {
            let b = lits[0][i];
            lits.iter().skip(1).all(|l| l[i] == b)
        })
        .count();
    if n == 0 {
        return lifted;
    }
    lifted.push(Node::Literal(lits[0][..n].to_vec()));
    for s in seqs.iter_mut() {
        let Node::Literal(b) = &mut s[0] else {
            unreachable!("checked above");
        };
        b.drain(..n);
        if b.is_empty() {
            s.remove(0);
        }
    }
    lifted
}

fn lift_suffix(seqs: &mut [Vec<Node>]) -> Vec<Node> {
    // Build outermost-first, then reverse once at the end so the caller
    // sees natural inner→outer order.
    let mut lifted_reverse = Vec::new();

    // Phase 1: atomic fold groups at the trailing edge.
    loop {
        let size = fold_group_at_edge(seqs[0].iter().rev());
        if size == 0 {
            break;
        }
        let len0 = seqs[0].len();
        let tail = &seqs[0][len0 - size..];
        let same = seqs.iter().skip(1).all(|s| {
            fold_group_at_edge(s.iter().rev()) == size
                && s[s.len() - size..]
                    .iter()
                    .zip(tail)
                    .all(|(x, y)| node_eq(x, y))
        });
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
        let lits: Vec<&[u8]> = seqs
            .iter()
            .map(|s| match s.last().unwrap() {
                Node::Literal(b) => b.as_slice(),
                _ => unreachable!("checked above"),
            })
            .collect();
        let min = lits.iter().map(|l| l.len()).min().unwrap_or(0);
        let n = (0..min)
            .take_while(|&i| {
                let b = lits[0][lits[0].len() - 1 - i];
                lits.iter().skip(1).all(|l| l[l.len() - 1 - i] == b)
            })
            .count();
        if n > 0 {
            lifted_reverse.push(Node::Literal(lits[0][lits[0].len() - n..].to_vec()));
            for s in seqs.iter_mut() {
                let last = s.len() - 1;
                let Node::Literal(b) = &mut s[last] else {
                    unreachable!("checked above");
                };
                b.truncate(b.len() - n);
                if b.is_empty() {
                    s.pop();
                }
            }
        }
    }

    lifted_reverse.reverse();
    lifted_reverse
}
