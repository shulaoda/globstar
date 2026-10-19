//! The parser's shape tests live in `fixtures/compiler-stages.tsv`, shared
//! with the JS runtime; this pins the one thing the fixture does not
//! record, the count of leading `!`.
use globstar::parser::parse;

#[test]
fn leading_bangs_are_counted() {
    for (pattern, count, negated) in [
        ("foo", 0, false),
        ("!foo", 1, true),
        ("!!(foo)", 2, false),
        ("!!!foo", 3, true),
    ] {
        let ast = parse(pattern.as_bytes()).unwrap();
        assert_eq!(ast.negation_count, count, "{pattern}");
        assert_eq!(ast.is_negated(), negated, "{pattern}");
    }
}
