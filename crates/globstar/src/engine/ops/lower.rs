//! AST to linear-op lowering.

use crate::ast::Node;

use super::ir::{Op, OpProgram};
use super::normalize::{
    apply_leading_seps_at_start, distribute_seps, fold_globstars_inplace, needs_sep_distribution,
};

/// Lower an AST into one normalized [`OpProgram`]. Brace alternatives remain
/// nested, so ordinary in-segment braces never incur cartesian expansion.
pub fn lower(node: &Node, case_insensitive: bool) -> OpProgram {
    let mut ops = Vec::new();
    if needs_sep_distribution(node) {
        let distributed = distribute_seps(node.clone());
        lower_into(&distributed, &mut ops, case_insensitive);
    } else {
        lower_into(node, &mut ops, case_insensitive);
    }
    finish(ops, case_insensitive)
}

fn finish(mut ops: Vec<Op>, case_insensitive: bool) -> OpProgram {
    fold_globstars_inplace(&mut ops);
    apply_leading_seps_at_start(&mut ops);
    OpProgram::from_normalized(ops, case_insensitive)
}

fn lower_into(node: &Node, out: &mut Vec<Op>, case_insensitive: bool) {
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
            for child in children {
                lower_into(child, out, case_insensitive);
            }
        }
        Node::Brace(branches) => {
            let mut lowered = Vec::with_capacity(branches.len());
            for branch in branches {
                let mut branch_ops = Vec::new();
                lower_into(branch, &mut branch_ops, case_insensitive);
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
