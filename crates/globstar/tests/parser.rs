//! Integration tests for the parser module (migrated from src/parser.rs).

use globstar::GlobError;
use globstar::ast::{Ast, Node};
use globstar::parser::parse;

fn p(s: &str) -> Ast {
    parse(s.as_bytes()).expect("parse should succeed")
}

fn err(s: &str) -> GlobError {
    parse(s.as_bytes()).expect_err("parse should fail")
}

#[test]
fn empty() {
    assert_eq!(parse(b""), Err(GlobError::Empty));
}

#[test]
fn pure_literal() {
    let a = p("foo");
    assert_eq!(a.negation_count, 0);
    assert_eq!(a.body, Node::Literal(b"foo".to_vec()));
}

#[test]
fn literal_with_separator() {
    let a = p("src/main.rs");
    assert!(a.body.is_pure_literal());
    assert_eq!(
        a.body.to_literal_bytes().as_deref(),
        Some(b"src/main.rs".as_ref())
    );
}

#[test]
fn star_alone() {
    let a = p("*");
    assert_eq!(a.body, Node::Star);
}

#[test]
fn star_extension() {
    let a = p("*.ts");
    match a.body {
        Node::Concat(xs) => {
            assert_eq!(xs.len(), 2);
            assert_eq!(xs[0], Node::Star);
            assert_eq!(xs[1], Node::Literal(b".ts".to_vec()));
        }
        _ => panic!("expected Concat, got {:?}", a.body),
    }
}

#[test]
fn globstar_basic() {
    let a = p("**");
    assert_eq!(a.body, Node::Globstar);
}

#[test]
fn globstar_in_path() {
    let a = p("src/**/*.ts");
    match a.body {
        Node::Concat(xs) => {
            assert_eq!(xs[0], Node::Literal(b"src".to_vec()));
            assert_eq!(xs[1], Node::Separator);
            assert_eq!(xs[2], Node::Globstar);
            assert_eq!(xs[3], Node::Separator);
            assert_eq!(xs[4], Node::Star);
            assert_eq!(xs[5], Node::Literal(b".ts".to_vec()));
        }
        _ => panic!("expected Concat"),
    }
}

#[test]
fn degenerate_globstar() {
    // `a**b` is not a real globstar; the second `*` should degrade.
    let a = p("a**b");
    match a.body {
        Node::Concat(xs) => {
            assert_eq!(xs[0], Node::Literal(b"a".to_vec()));
            // Both `*`s become Star (collapse to single * semantically later).
            assert_eq!(xs[1], Node::Star);
            assert_eq!(xs[2], Node::Star);
            assert_eq!(xs[3], Node::Literal(b"b".to_vec()));
        }
        _ => panic!("expected Concat"),
    }
}

#[test]
fn class_basic() {
    let a = p("[abc]");
    match a.body {
        Node::Class(c) => {
            assert!(!c.negated);
            assert_eq!(c.items.len(), 3);
            assert!(c.matches(b'a'));
            assert!(c.matches(b'b'));
            assert!(c.matches(b'c'));
            assert!(!c.matches(b'd'));
        }
        _ => panic!("expected Class"),
    }
}

#[test]
fn class_range() {
    let a = p("[a-z]");
    match a.body {
        Node::Class(c) => {
            assert!(c.matches(b'a'));
            assert!(c.matches(b'm'));
            assert!(c.matches(b'z'));
            assert!(!c.matches(b'A'));
            assert!(!c.matches(b'0'));
        }
        _ => panic!("expected Class"),
    }
}

#[test]
fn class_negated() {
    let a = p("[!abc]");
    match a.body {
        Node::Class(c) => {
            assert!(c.negated);
            assert!(!c.matches(b'a'));
            assert!(c.matches(b'd'));
        }
        _ => panic!("expected Class"),
    }
}

#[test]
fn class_caret_negation() {
    let a = p("[^abc]");
    match a.body {
        Node::Class(c) => assert!(c.negated),
        _ => panic!("expected Class"),
    }
}

#[test]
fn brace_basic() {
    let a = p("{a,b,c}");
    match a.body {
        Node::Brace(branches) => {
            assert_eq!(branches.len(), 3);
            assert_eq!(branches[0], Node::Literal(b"a".to_vec()));
            assert_eq!(branches[1], Node::Literal(b"b".to_vec()));
            assert_eq!(branches[2], Node::Literal(b"c".to_vec()));
        }
        _ => panic!("expected Brace"),
    }
}

#[test]
fn brace_nested() {
    let a = p("{a,{b,c}}");
    match a.body {
        Node::Brace(branches) => {
            assert_eq!(branches.len(), 2);
            assert!(matches!(branches[1], Node::Brace(_)));
        }
        _ => panic!("expected Brace"),
    }
}

#[test]
fn brace_single_is_literal() {
    // `{a}` should be the literal `{a}` (GLOB_SPEC §7.4).
    let a = p("{a}");
    assert!(matches!(a.body, Node::Concat(_)));
    // First child should be `{`.
    if let Node::Concat(xs) = &a.body {
        assert_eq!(xs[0], Node::Literal(b"{".to_vec()));
    }
}

#[test]
fn extglob_negate() {
    // Leading `!` = negation; `(foo)` = literal bytes.
    let a = p("!(foo)");
    assert_eq!(a.negation_count, 1);
    assert_eq!(a.body, Node::Literal(b"(foo)".to_vec()));
}

#[test]
fn extglob_at_with_alternatives() {
    // `@ ( | )` are all literal bytes.
    let a = p("@(a|b|c)");
    assert_eq!(a.negation_count, 0);
    assert_eq!(a.body, Node::Literal(b"@(a|b|c)".to_vec()));
}

#[test]
fn negation_prefix() {
    let a = p("!foo");
    assert_eq!(a.negation_count, 1);
    assert!(a.is_negated());
    assert_eq!(a.body, Node::Literal(b"foo".to_vec()));
}

#[test]
fn double_negation() {
    let a = p("!!foo");
    assert_eq!(a.negation_count, 2);
    assert!(!a.is_negated());
}

#[test]
fn negation_then_extglob_is_extglob() {
    // One leading `!` + literal body.
    let a = p("!(foo)");
    assert_eq!(a.negation_count, 1);
    assert_eq!(a.body, Node::Literal(b"(foo)".to_vec()));
}

#[test]
fn double_bang_then_extglob() {
    // `!!` counts as 2 negations (even → not negated); body literal.
    let a = p("!!(foo)");
    assert_eq!(a.negation_count, 2);
    assert!(!a.is_negated());
    assert_eq!(a.body, Node::Literal(b"(foo)".to_vec()));
}

#[test]
fn escape_metachar() {
    let a = p("\\*");
    assert_eq!(a.body, Node::Literal(b"*".to_vec()));
}

#[test]
fn escape_lenient() {
    let a = p("\\a");
    assert_eq!(a.body, Node::Literal(b"a".to_vec()));
}

#[test]
fn escape_at_end() {
    assert_eq!(err("foo\\"), GlobError::TrailingBackslash);
}

#[test]
fn unterminated_class() {
    assert!(matches!(err("[abc"), GlobError::UnterminatedClass { .. }));
}

#[test]
fn unterminated_brace() {
    assert!(matches!(err("{a,b"), GlobError::UnterminatedBrace { .. }));
}

#[test]
fn unterminated_extglob() {
    // `@(a` is a literal three-byte sequence — no parse error.
    let a = p("@(a");
    assert_eq!(a.body, Node::Literal(b"@(a".to_vec()));
}

#[test]
fn posix_first_bracket_literal_unterminated() {
    // POSIX: leading `]` after `[` (or `[!`/`[^`) is a literal class member,
    // so `[]`, `[!]`, `[^]` all decay to "literal ] + no closer" and must
    // surface as `UnterminatedClass`, not `EmptyClass`/`EmptyNegationClass`.
    assert!(matches!(err("["), GlobError::UnterminatedClass { .. }));
    assert!(matches!(err("[]"), GlobError::UnterminatedClass { .. }));
    assert!(matches!(err("[!]"), GlobError::UnterminatedClass { .. }));
    assert!(matches!(err("[^]"), GlobError::UnterminatedClass { .. }));
}

#[test]
fn posix_first_bracket_literal_valid() {
    // POSIX: the leading `]` becomes a literal class member. These compile.
    use globstar::Glob;
    // `[]]` = class {]}
    let g = Glob::new("a[]]b").unwrap();
    assert!(g.is_match(b"a]b"));
    assert!(!g.is_match(b"aab"));
    // `[]-]` = class {`]`, `-`}
    let g = Glob::new("a[]-]b").unwrap();
    assert!(g.is_match(b"a]b"));
    assert!(g.is_match(b"a-b"));
    assert!(!g.is_match(b"aab"));
    // Negated: `[!]abc]` would be negated class {`]`, a, b, c}.
    let g = Glob::new("[!]]").unwrap();
    assert!(!g.is_match(b"]"));
    assert!(g.is_match(b"x"));
}

#[test]
fn separator_in_extglob() {
    // `@(src/lib)` parses as literal `@(src` + `/` + literal `lib)`.
    let a = p("@(src/lib)");
    match a.body {
        Node::Concat(xs) => {
            assert_eq!(xs[0], Node::Literal(b"@(src".to_vec()));
            assert_eq!(xs[1], Node::Separator);
            assert_eq!(xs[2], Node::Literal(b"lib)".to_vec()));
        }
        _ => panic!("expected Concat, got {:?}", a.body),
    }
}

#[test]
fn invalid_range() {
    assert!(matches!(err("[z-a]"), GlobError::InvalidRange { .. }));
}

#[test]
fn collapse_double_globstar() {
    let a = p("a/**/**/b");
    // Should collapse to a single Globstar.
    match a.body {
        Node::Concat(xs) => {
            let globstar_count = xs.iter().filter(|n| matches!(n, Node::Globstar)).count();
            assert_eq!(globstar_count, 1);
        }
        _ => panic!("expected Concat"),
    }
}
