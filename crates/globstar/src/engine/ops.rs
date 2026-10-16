//! The linear IR between the parser and the segment engine, and the
//! AST → ops lowering that produces it.

use crate::ast::{CharClass, Node};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Match a literal byte sequence verbatim.
    Lit(Vec<u8>),
    /// Match a single non-separator byte.
    AnyChar,
    /// Match zero or more non-separator bytes.
    Star,
    /// Match one byte against the class.
    Class(CharClass),
    /// Match exactly one path separator.
    Sep,
    /// Match one or more path separators at a lenient globstar boundary.
    SepRun,
    /// `(?:[^/]+/+)*`, used for leading and middle `**/`. Every absorbed
    /// segment is nonempty, empty segments come from the boundary ops.
    OptSegmentsSlash,
    /// `/.*`, used for strict trailing `/**`.
    SlashAnything,
    /// `.*`, used for a bare `**`.
    GlobstarAny,
    /// Zero or more platform separators: for pattern-head `**/`, and for a
    /// `**/` that opens a brace branch after a brace that took the `/`.
    LeadingSeps,
    /// Brace alternation. Branches remain nested rather than cartesian-expanded.
    Alternation(Vec<Vec<Op>>),
}

#[derive(Debug, Clone)]
pub struct OpProgram {
    pub(crate) ops: Vec<Op>,
    pub(crate) case_insensitive: bool,
}

impl OpProgram {
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }
}

/// Lower an AST into one normalized [`OpProgram`].
pub fn lower(node: Node, case_insensitive: bool) -> OpProgram {
    let mut ops = Vec::with_capacity(match &node {
        Node::Concat(children) => children.len() + 1,
        _ => 2,
    });
    lower_into(node, &mut ops, case_insensitive);
    lead_seps(&mut ops);
    OpProgram {
        ops,
        case_insensitive,
    }
}

fn lower_into(node: Node, out: &mut Vec<Op>, case_insensitive: bool) {
    match node {
        Node::Literal(bytes) => push_op(out, Op::Lit(bytes)),
        Node::Separator => push_op(out, Op::Sep),
        Node::AnyChar => push_op(out, Op::AnyChar),
        Node::Star => push_op(out, Op::Star),
        Node::Globstar => out.push(Op::GlobstarAny),
        Node::Class(class) => {
            let class = if case_insensitive {
                class.expanded_ascii_case_insensitive()
            } else {
                class
            };
            push_op(out, Op::Class(class));
        }
        Node::Concat(children) => {
            let mut children = children.into_iter().peekable();
            while let Some(mut child) = children.next() {
                if matches!(child, Node::Globstar) {
                    // `**/` is `OptSegmentsSlash`, and the `/` before it (a
                    // brace's trailing ones too: `{src/,lib/}**/x`, §7.0 +
                    // §12.3) is lenient; `/**` is `SlashAnything`; a bare
                    // `**` is `GlobstarAny`.
                    if children.next_if(|n| matches!(n, Node::Separator)).is_some() {
                        match out.last_mut() {
                            Some(op @ Op::Sep) => *op = Op::SepRun,
                            Some(Op::Alternation(branches)) => upgrade_trailing_seps(branches),
                            _ => {}
                        }
                        out.push(Op::OptSegmentsSlash);
                    } else if let Some(op @ Op::Sep) = out.last_mut() {
                        *op = Op::SlashAnything;
                    } else {
                        out.push(Op::GlobstarAny);
                    }
                    continue;
                }
                let Node::Brace(branches) = &child else {
                    lower_into(child, out, case_insensitive);
                    continue;
                };
                // A brace with a `**` at an edge takes the `/` beside it
                // into every branch, so the `**/` or `/**` fold happens
                // inside the branch: `{a/**,b}/x` lowers as `{a/**/,b/}x`.
                let prev =
                    matches!(out.last(), Some(Op::Sep)) && branches.iter().any(leads_globstar);
                let next = matches!(children.peek(), Some(Node::Separator))
                    && branches.iter().any(trails_globstar);
                if prev {
                    out.pop();
                }
                if next {
                    children.next();
                }
                if prev || next {
                    child = with_seps(child, prev, next);
                }
                // Two braces around one `/`, both with a `**` at that edge:
                // the first took the `/`, so a `**/` opening a branch of
                // the second keeps its lenient boundary (§12.3) by taking
                // any further separators itself.
                let after_brace = matches!(out.last(), Some(Op::Alternation(_)));
                lower_into(child, out, case_insensitive);
                if after_brace && let Some(Op::Alternation(branches)) = out.last_mut() {
                    branches.iter_mut().for_each(lead_seps);
                }
            }
        }
        Node::Brace(branches) => {
            let mut lowered = Vec::with_capacity(branches.len());
            for branch in branches {
                let mut branch_ops = Vec::new();
                lower_into(branch, &mut branch_ops, case_insensitive);
                lowered.push(branch_ops);
            }
            push_op(out, Op::Alternation(lowered));
        }
    }
}

/// `brace` with a `/` before and/or after every branch.
fn with_seps(brace: Node, prev: bool, next: bool) -> Node {
    let Node::Brace(branches) = brace else {
        unreachable!("only a brace takes a `/`")
    };
    let wrap = |branch| {
        let mut seq = Vec::with_capacity(3);
        if prev {
            seq.push(Node::Separator);
        }
        match branch {
            Node::Concat(children) => seq.extend(children),
            other => seq.push(other),
        }
        if next {
            seq.push(Node::Separator);
        }
        Node::Concat(seq)
    };
    Node::Brace(branches.into_iter().map(wrap).collect())
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

/// A `**/` that opens the program, or a branch at its head, also takes
/// the leading separators (§8.5).
fn lead_seps(ops: &mut Vec<Op>) {
    match ops.first_mut() {
        Some(Op::OptSegmentsSlash) => ops.insert(0, Op::LeadingSeps),
        Some(Op::Alternation(branches)) => branches.iter_mut().for_each(lead_seps),
        _ => {}
    }
}

fn upgrade_trailing_seps(branches: &mut [Vec<Op>]) {
    for branch in branches {
        match branch.last_mut() {
            Some(op) if matches!(op, Op::Sep) => *op = Op::SepRun,
            Some(Op::Alternation(inner)) => upgrade_trailing_seps(inner),
            _ => {}
        }
    }
}
