//! What a pattern compiles to, stage by stage, on the tables the JS
//! suite reads too: its AST (`fixtures/compiler-stages.tsv`) and the
//! shape of its matcher (`fixtures/engine-shapes.tsv`). A slower routing
//! still answers right, so only the shapes see it.

use std::fmt::Write as _;

use globstar::Glob;
use globstar::parse::ast::Node;
use globstar::parse::parse;

const AST_CASES: &str = include_str!("../../../fixtures/compiler-stages.tsv");
const SHAPE_CASES: &str = include_str!("../../../fixtures/engine-shapes.tsv");

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
    for (line_no, line) in AST_CASES.lines().enumerate() {
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

#[test]
fn patterns_compile_to_their_shapes() {
    let mut rows = 0;
    for line in SHAPE_CASES.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let want = fields.next().unwrap();
        let patterns: Vec<&str> = fields.collect();
        let glob = match patterns.as_slice() {
            [one] => Glob::new(one),
            many => Glob::union(many.iter().copied()),
        }
        .unwrap();
        assert_eq!(glob.shape(), want, "{:.60}", patterns.join(" + "));
        rows += 1;
    }
    assert!(rows >= 20, "only {rows} rows");
}

// Past 64 states the graph NFA keeps dense rows while they fit 4 KiB (170
// states) and no position leads to more than 48 literals.
#[test]
fn a_graph_past_64_states_keeps_dense_rows_within_bounds() {
    let lits = |n: usize| -> Vec<String> { (0..n).map(|i| format!("dir{i}/file{i}")).collect() };
    let wilds =
        |n: usize| -> Vec<String> { (0..n).map(|i| format!("src/*{i}/**/?*b{i}/*.ts")).collect() };
    let paths = [
        "",
        "x",
        "dir3",
        "dir3/file3",
        "dir3/file4",
        "dir3/file3/x",
        "dir47/file47",
        "dir48/file48",
        "dir48",
        "src",
        "src/a7",
        "src/a7/x",
        "src/a7/x/yb7/z.ts",
        "src/a7/yb7/z.ts",
        "src/a7/b7/z.ts",
        "src/a7/x/y/wb7/z.ts",
        "src/a54/q/wb54/k.ts",
        "src/a55/q/wb55/k.ts",
        "src/a7/x/yb7/z.js",
    ];
    for (members, nfa) in [
        (lits(48), "none"),
        (lits(49), "none"),
        (wilds(55), "wide"),
        (wilds(56), "sparse"),
    ] {
        let union = Glob::union(members.iter().map(String::as_str)).unwrap();
        let shape = union.shape();
        assert!(
            shape.ends_with(&format!("nfa {nfa}")),
            "{} x{}",
            members[0],
            members.len()
        );
        let each: Vec<Glob> = members.iter().map(|p| Glob::new(p).unwrap()).collect();
        for path in paths {
            let p = path.as_bytes();
            let any = |f: &dyn Fn(&Glob) -> bool| each.iter().any(f);
            assert_eq!(union.is_match(p), any(&|g| g.is_match(p)), "{path:?}");
            let dir = union.match_dir(p);
            assert_eq!(
                dir.matches(),
                any(&|g| g.match_dir(p).matches()),
                "{path:?}"
            );
            let below = any(&|g| g.match_dir(p).may_match_below());
            assert_eq!(dir.may_match_below(), below, "{path:?}");
        }
    }
}
