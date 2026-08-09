use crate::ast::Node;

use super::ir::{Op, OpProgram};

/// Lower an AST into one normalized [`OpProgram`]. Brace alternatives remain
/// nested, so ordinary in-segment braces never incur cartesian expansion.
pub fn lower(node: &Node, case_insensitive: bool) -> OpProgram {
    let mut ops = Vec::new();
    let mut needs_distribution = false;
    lower_into(node, &mut ops, case_insensitive, &mut needs_distribution);
    if needs_distribution {
        ops.clear();
        let distributed = distribute_seps(node.clone());
        lower_into(
            &distributed,
            &mut ops,
            case_insensitive,
            &mut needs_distribution,
        );
    }
    fold_globstars_inplace(&mut ops);
    apply_leading_seps_at_start(&mut ops);
    OpProgram::from_normalized(ops, case_insensitive)
}

fn lower_into(
    node: &Node,
    out: &mut Vec<Op>,
    case_insensitive: bool,
    needs_distribution: &mut bool,
) {
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
                lower_into(
                    branch,
                    &mut branch_ops,
                    case_insensitive,
                    needs_distribution,
                );
                fold_globstars_inplace(&mut branch_ops);
                lowered.push(branch_ops);
            }
            push_op(out, Op::Alternation(lowered));
        }
    }
}

fn push_op(out: &mut Vec<Op>, op: Op) {
    match op {
        Op::Star if matches!(out.last(), Some(Op::Star)) => {}
        Op::Lit(bytes) => {
            if let Some(Op::Lit(previous)) = out.last_mut() {
                previous.extend_from_slice(&bytes);
            } else {
                out.push(Op::Lit(bytes));
            }
        }
        other => out.push(other),
    }
}

fn leads_globstar(node: &Node) -> bool {
    match node {
        Node::Globstar => true,
        Node::Concat(children) => children.first().is_some_and(leads_globstar),
        Node::Brace(branches) => branches.iter().any(leads_globstar),
        _ => false,
    }
}

fn trails_globstar(node: &Node) -> bool {
    match node {
        Node::Globstar => true,
        Node::Concat(children) => children.last().is_some_and(trails_globstar),
        Node::Brace(branches) => branches.iter().any(trails_globstar),
        _ => false,
    }
}

fn apply_leading_seps_at_start(ops: &mut Vec<Op>) {
    match ops.first_mut() {
        Some(Op::OptSegmentsSlash) => ops.insert(0, Op::LeadingSeps),
        Some(Op::Alternation(branches)) => {
            for branch in branches {
                apply_leading_seps_at_start(branch);
            }
        }
        _ => {}
    }
}

fn distribute_seps(node: Node) -> Node {
    match node {
        Node::Concat(children) => {
            let mut out = Vec::with_capacity(children.len());
            let mut iter = children.into_iter().peekable();
            while let Some(child) = iter.next() {
                let Node::Brace(branches) = child else {
                    out.push(distribute_seps(child));
                    continue;
                };
                let prev_is_sep = matches!(out.last(), Some(Node::Separator));
                let prev_sep_owned_by_globstar =
                    prev_is_sep && matches!(out.iter().rev().nth(1), Some(Node::Globstar));
                let absorb_prev = prev_is_sep
                    && !prev_sep_owned_by_globstar
                    && branches.iter().any(leads_globstar);
                let absorb_next = matches!(iter.peek(), Some(Node::Separator))
                    && branches.iter().any(trails_globstar);
                if !absorb_prev && !absorb_next {
                    out.push(distribute_seps(Node::Brace(branches)));
                    continue;
                }
                if absorb_prev {
                    out.pop();
                }
                if absorb_next {
                    iter.next();
                }
                let branches = branches
                    .into_iter()
                    .map(|branch| {
                        let mut sequence = Vec::with_capacity(3);
                        if absorb_prev {
                            sequence.push(Node::Separator);
                        }
                        sequence.push(branch);
                        if absorb_next {
                            sequence.push(Node::Separator);
                        }
                        distribute_seps(Node::Concat(sequence))
                    })
                    .collect();
                out.push(Node::Brace(branches));
            }
            Node::Concat(out)
        }
        Node::Brace(branches) => Node::Brace(branches.into_iter().map(distribute_seps).collect()),
        other => other,
    }
}

fn fold_globstars_inplace(ops: &mut Vec<Op>) {
    let mut write = 0usize;
    let mut read = 0usize;
    while read < ops.len() {
        if matches!(ops[read], Op::Globstar) && matches!(ops.get(read + 1), Some(Op::Sep)) {
            ops[write] = Op::OptSegmentsSlash;
            read += 2;
            write += 1;
        } else {
            if read != write {
                ops.swap(write, read);
            }
            read += 1;
            write += 1;
        }
    }
    ops.truncate(write);

    for i in 0..ops.len().saturating_sub(1) {
        if matches!(ops[i], Op::Sep) && matches!(ops[i + 1], Op::OptSegmentsSlash) {
            ops[i] = Op::SepRun;
        }
    }

    write = 0;
    read = 0;
    while read < ops.len() {
        if matches!(ops[read], Op::Sep) && matches!(ops.get(read + 1), Some(Op::Globstar)) {
            ops[write] = Op::SlashAnything;
            read += 2;
            write += 1;
        } else {
            if read != write {
                ops.swap(write, read);
            }
            read += 1;
            write += 1;
        }
    }
    ops.truncate(write);

    for op in ops {
        if matches!(op, Op::Globstar) {
            *op = Op::GlobstarAny;
        }
    }
}
