//! Integration tests for the ast module (migrated from src/ast.rs).

use globstar::ast::{CharClass, ClassItem, Node};
use globstar::parser::parse;

#[test]
fn pure_literal_detection() {
    let n = Node::Concat(vec![
        Node::Literal(b"src".to_vec()),
        Node::Separator,
        Node::Literal(b"main.rs".to_vec()),
    ]);
    assert!(n.is_pure_literal());
    assert_eq!(
        n.to_literal_bytes().as_deref(),
        Some(b"src/main.rs".as_ref())
    );
}

#[test]
fn star_is_not_pure_literal() {
    let n = Node::Concat(vec![Node::Star, Node::Literal(b".rs".to_vec())]);
    assert!(!n.is_pure_literal());
    assert!(n.to_literal_bytes().is_none());
}

#[test]
fn extglob_detection() {
    // `!(foo)` = leading `!` negation + literal `(foo)`.
    let ast = parse(b"!(foo)").expect("parse");
    assert_eq!(ast.negation_count, 1);
    assert!(ast.body.is_pure_literal());
    assert_eq!(
        ast.body.to_literal_bytes().as_deref(),
        Some(b"(foo)".as_ref())
    );
}

#[test]
fn class_excludes_separator() {
    let c = CharClass {
        negated: true,
        items: vec![ClassItem::Byte(b'x')],
    };
    // Negated class should NOT match `/`.
    assert!(!c.matches(b'/'));
    assert!(c.matches(b'a'));
    assert!(!c.matches(b'x'));
}
