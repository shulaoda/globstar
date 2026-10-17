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
    /// `**/`: zero or more whole segments, empty ones included, so the
    /// separators around it are lenient (§12.3).
    OptSegmentsSlash,
    /// A `**` that ends the pattern: the rest of the path.
    GlobstarAny,
    /// Brace alternation. Branches remain nested rather than cartesian-expanded.
    Alternation(Vec<Vec<Op>>),
}

/// Lower an AST into ops: adjacent literals joined, a `*` run one `*`,
/// and every `**` folded with the `/` after it.
pub fn lower(node: Node) -> Vec<Op> {
    let mut ops = Vec::with_capacity(match &node {
        Node::Concat(children) => children.len(),
        _ => 1,
    });
    lower_into(node, &mut ops);
    ops
}

fn lower_into(node: Node, out: &mut Vec<Op>) {
    match node {
        Node::Literal(bytes) => push_op(out, Op::Lit(bytes)),
        Node::Separator => push_op(out, Op::Sep),
        Node::AnyChar => push_op(out, Op::AnyChar),
        Node::Star => push_op(out, Op::Star),
        Node::Globstar => out.push(Op::GlobstarAny),
        Node::Class(class) => push_op(out, Op::Class(class)),
        Node::Concat(children) => {
            let mut children = children.into_iter().peekable();
            while let Some(child) = children.next() {
                let sep_next = matches!(children.peek(), Some(Node::Separator));
                match child {
                    // `**/` is `OptSegmentsSlash`, a bare `**` `GlobstarAny`.
                    Node::Globstar if sep_next => {
                        children.next();
                        out.push(Op::OptSegmentsSlash);
                    }
                    // A brace with a `**` at its end takes the `/` after it
                    // into every branch, so that `**` folds there too:
                    // `{a/**,b}/x` lowers as `{a/**/,b/}x`.
                    Node::Brace(branches) if sep_next && branches.iter().any(trails_globstar) => {
                        children.next();
                        let with_sep = |branch| {
                            let mut seq = match branch {
                                Node::Concat(children) => children,
                                other => vec![other],
                            };
                            seq.push(Node::Separator);
                            Node::Concat(seq)
                        };
                        lower_into(
                            Node::Brace(branches.into_iter().map(with_sep).collect()),
                            out,
                        );
                    }
                    child => lower_into(child, out),
                }
            }
        }
        Node::Brace(branches) => {
            let mut lowered = Vec::with_capacity(branches.len());
            for branch in branches {
                let mut branch_ops = Vec::new();
                lower_into(branch, &mut branch_ops);
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

fn trails_globstar(node: &Node) -> bool {
    match node {
        Node::Globstar => true,
        Node::Concat(children) => children.last().is_some_and(trails_globstar),
        Node::Brace(branches) => branches.iter().any(trails_globstar),
        _ => false,
    }
}
