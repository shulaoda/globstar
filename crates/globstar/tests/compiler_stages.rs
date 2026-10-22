use std::fmt::Write as _;

use globstar::parse::ast::Node;
use globstar::parse::parse;

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
            for (i, &(lo, hi)) in class.ranges.iter().enumerate() {
                if i != 0 {
                    out.push(',');
                }
                bytes(out, &[lo]);
                if hi != lo {
                    out.push('-');
                    bytes(out, &[hi]);
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

#[test]
fn parser_matches_shared_golden_cases() {
    for (line_no, line) in CASES.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let [pattern, expected_ast] = line
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or_else(|_| panic!("bad fixture line {}", line_no + 1));
        let parsed = parse(pattern.as_bytes()).expect("fixture pattern parses");
        let mut actual_ast = String::new();
        ast_dump(&parsed.body, &mut actual_ast);
        assert_eq!(actual_ast, expected_ast, "AST for {pattern:?}");
    }
}
