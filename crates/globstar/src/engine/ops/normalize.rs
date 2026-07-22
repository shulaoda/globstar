//! Structural normalization required before matcher backends see the IR.

use crate::ast::Node;

use super::ir::Op;

pub(super) fn apply_leading_seps_at_start(ops: &mut Vec<Op>) {
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

pub(super) fn needs_sep_distribution(node: &Node) -> bool {
    match node {
        Node::Concat(children) => {
            for (i, child) in children.iter().enumerate() {
                if let Node::Brace(branches) = child {
                    let prev_sep = i > 0 && matches!(children[i - 1], Node::Separator);
                    let next_sep = matches!(children.get(i + 1), Some(Node::Separator));
                    if (prev_sep && branches.iter().any(leads_globstar))
                        || (next_sep && branches.iter().any(trails_globstar))
                    {
                        return true;
                    }
                }
                if needs_sep_distribution(child) {
                    return true;
                }
            }
            false
        }
        Node::Brace(branches) => branches.iter().any(needs_sep_distribution),
        _ => false,
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

pub(super) fn distribute_seps(node: Node) -> Node {
    match node {
        Node::Concat(children) => {
            let mut out = Vec::with_capacity(children.len());
            let mut iter = children.into_iter().peekable();
            while let Some(child) = iter.next() {
                let Node::Brace(branches) = child else {
                    out.push(distribute_seps(child));
                    continue;
                };
                let absorb_prev = matches!(out.last(), Some(Node::Separator))
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

/// Fold raw globstar/separator adjacency into the four semantic op forms.
pub(super) fn fold_globstars_inplace(ops: &mut Vec<Op>) {
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
