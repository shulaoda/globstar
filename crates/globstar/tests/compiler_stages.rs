use std::fmt::Write as _;

use globstar::ast::{ClassItem, Node};
use globstar::engine::ops::{Op, lower, lower_owned};
use globstar::parser::parse;

const CASES: &str = include_str!("../../../fixtures/compiler-stages.tsv");

fn bytes(out: &mut String, value: &[u8]) {
    for &b in value {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'(' | b')' | b',' | b'|' | b'[' | b']' => {
                let _ = write!(out, "\\x{b:02x}");
            }
            0x20..=0x7e => out.push(b as char),
            _ => {
                let _ = write!(out, "\\x{b:02x}");
            }
        }
    }
}

fn ast_dump(node: &Node, out: &mut String) {
    match node {
        Node::Concat(children) => {
            out.push_str("C(");
            for (i, child) in children.iter().enumerate() {
                if i != 0 {
                    out.push(',');
                }
                ast_dump(child, out);
            }
            out.push(')');
        }
        Node::Literal(value) => {
            out.push_str("L(");
            bytes(out, value);
            out.push(')');
        }
        Node::Separator => out.push('S'),
        Node::AnyChar => out.push('Q'),
        Node::Star => out.push('T'),
        Node::Globstar => out.push('G'),
        Node::Class(class) => {
            out.push_str(if class.negated { "N(" } else { "K(" });
            for (i, item) in class.items.iter().enumerate() {
                if i != 0 {
                    out.push(',');
                }
                match item {
                    ClassItem::Byte(b) => bytes(out, &[*b]),
                    ClassItem::Range(lo, hi) => {
                        bytes(out, &[*lo]);
                        out.push('-');
                        bytes(out, &[*hi]);
                    }
                }
            }
            out.push(')');
        }
        Node::Brace(branches) => {
            out.push_str("B(");
            for (i, branch) in branches.iter().enumerate() {
                if i != 0 {
                    out.push('|');
                }
                ast_dump(branch, out);
            }
            out.push(')');
        }
    }
}

fn ops_dump(ops: &[Op], out: &mut String) {
    for (i, op) in ops.iter().enumerate() {
        if i != 0 {
            out.push(',');
        }
        match op {
            Op::Lit(value) => {
                out.push_str("L(");
                bytes(out, value);
                out.push(')');
            }
            Op::AnyChar => out.push('Q'),
            Op::Star => out.push('T'),
            Op::Class(_) => out.push('K'),
            Op::Sep => out.push('S'),
            Op::SepRun => out.push('R'),
            Op::Globstar => out.push('G'),
            Op::OptSegmentsSlash => out.push('O'),
            Op::SlashAnything => out.push('A'),
            Op::GlobstarAny => out.push('Y'),
            Op::LeadingSeps => out.push('H'),
            Op::Alternation(branches) => {
                out.push_str("B(");
                for (j, branch) in branches.iter().enumerate() {
                    if j != 0 {
                        out.push('|');
                    }
                    out.push('[');
                    ops_dump(branch, out);
                    out.push(']');
                }
                out.push(')');
            }
        }
    }
}

#[test]
fn parser_and_lowering_match_shared_golden_cases() {
    for (line_no, line) in CASES.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let [pattern, expected_ast, expected_ops] = line
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or_else(|_| panic!("bad fixture line {}", line_no + 1));
        let parsed = parse(pattern.as_bytes()).expect("fixture pattern parses");
        let mut actual_ast = String::new();
        ast_dump(&parsed.body, &mut actual_ast);
        assert_eq!(actual_ast, expected_ast, "AST for {pattern:?}");

        let program = lower(&parsed.body, false);
        let mut actual_ops = String::new();
        ops_dump(program.ops(), &mut actual_ops);
        assert_eq!(actual_ops, expected_ops, "ops for {pattern:?}");

        let owned = lower_owned(parsed.body, false);
        let mut owned_ops = String::new();
        ops_dump(owned.ops(), &mut owned_ops);
        assert_eq!(owned_ops, expected_ops, "owned ops for {pattern:?}");
    }
}
