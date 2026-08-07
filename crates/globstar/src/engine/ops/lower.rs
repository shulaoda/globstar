//! AST to linear-op lowering.

use crate::ast::Node;

use super::ir::{Op, OpProgram};
use super::normalize::{
    apply_leading_seps_at_start, distribute_seps, fold_globstars_inplace, leads_globstar,
    trails_globstar,
};

/// Lower an AST into one normalized [`OpProgram`]. Brace alternatives remain
/// nested, so ordinary in-segment braces never incur cartesian expansion.
///
/// Optimistic single pass: [`lower_into`] emits ops directly and, in the
/// same walk, flags whether any brace edge abuts a separator — the sole
/// shape that needs §7 separator distribution. Only when that trips (rare)
/// do we redo on the distributed tree. The common pattern pays exactly one
/// walk, with no separate pre-check.
pub fn lower(node: &Node, case_insensitive: bool) -> OpProgram {
    let mut ops = Vec::new();
    let mut needs_distribution = false;
    lower_into(node, &mut ops, case_insensitive, &mut needs_distribution);
    if needs_distribution {
        // The flag is a conservative superset of what `distribute_seps`
        // actually rewrites — it deliberately leaves some adjacencies
        // alone (e.g. a `/` owned by a preceding `**`), so the flag can
        // re-trip on the distributed tree even though that tree is already
        // the fixpoint. Re-lower on it and ignore any re-trip.
        ops.clear();
        let distributed = distribute_seps(node.clone());
        lower_into(&distributed, &mut ops, case_insensitive, &mut needs_distribution);
    }
    finish(ops, case_insensitive)
}

fn finish(mut ops: Vec<Op>, case_insensitive: bool) -> OpProgram {
    fold_globstars_inplace(&mut ops);
    apply_leading_seps_at_start(&mut ops);
    OpProgram::from_normalized(ops, case_insensitive)
}

fn lower_into(node: &Node, out: &mut Vec<Op>, case_insensitive: bool, needs_distribution: &mut bool) {
    match node {
        Node::Literal(bytes) => push_op(out, Op::Lit(bytes.clone())),
        Node::Separator => push_op(out, Op::Sep),
        Node::AnyChar => push_op(out, Op::AnyChar),
        Node::Star => push_op(out, Op::Star),
        Node::Globstar => push_op(out, Op::Globstar),
        Node::Class(class) => {
            let class = if case_insensitive {
                class.expanded_ascii_case_insensitive()
            } else {
                class.clone()
            };
            push_op(out, Op::Class(class));
        }
        Node::Concat(children) => {
            for (i, child) in children.iter().enumerate() {
                // Flag a brace edge abutting a separator — the only shape
                // that needs §7 separator distribution. `lower` then redoes
                // the walk on the distributed tree. Detected here so the
                // common no-brace pattern needs no separate pre-check walk.
                if let Node::Brace(branches) = child {
                    let prev_sep = i > 0 && matches!(children[i - 1], Node::Separator);
                    let next_sep = matches!(children.get(i + 1), Some(Node::Separator));
                    if (prev_sep && branches.iter().any(leads_globstar))
                        || (next_sep && branches.iter().any(trails_globstar))
                    {
                        *needs_distribution = true;
                    }
                }
                lower_into(child, out, case_insensitive, needs_distribution);
            }
        }
        Node::Brace(branches) => {
            let mut lowered = Vec::with_capacity(branches.len());
            for branch in branches {
                let mut branch_ops = Vec::new();
                lower_into(branch, &mut branch_ops, case_insensitive, needs_distribution);
                fold_globstars_inplace(&mut branch_ops);
                lowered.push(branch_ops);
            }
            push_op(out, Op::Alternation(lowered));
        }
    }
}

/// Maintain the local canonical-form invariant while emitting ops.
fn push_op(out: &mut Vec<Op>, op: Op) {
    if matches!(op, Op::Star) && matches!(out.last(), Some(Op::Star)) {
        return;
    }
    if let Op::Lit(bytes) = op {
        if let Some(Op::Lit(previous)) = out.last_mut() {
            previous.extend_from_slice(&bytes);
        } else {
            out.push(Op::Lit(bytes));
        }
    } else {
        out.push(op);
    }
}
