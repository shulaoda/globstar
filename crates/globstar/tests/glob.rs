//! Integration tests for the Glob type (migrated from src/lib.rs).

use globstar::{Glob, Tier};

#[test]
fn classify_literal() {
    let g = Glob::new("README.md").unwrap();
    assert_eq!(g.tier(), Tier::Literal);
    // Glob::new succeeds → pattern is fully supported.
}

#[test]
fn classify_simple_wildcard() {
    let g = Glob::new("*.ts").unwrap();
    assert_eq!(g.tier(), Tier::SimpleWildcard);
    assert!(g.is_match(b"main.ts"));
    assert!(!g.is_match(b"main.rs"));
}

#[test]
fn classify_globstar() {
    let g = Glob::new("src/**/*.ts").unwrap();
    assert_eq!(g.tier(), Tier::Globstar);
}

#[test]
fn non_literal_patterns_use_segment_engine() {
    for pattern in ["*.ts", "src/**/*.ts", "**/*.{ts,tsx,js,jsx}"] {
        assert_eq!(Glob::new(pattern).unwrap().engine_name(), "Segment");
    }
}

#[test]
fn segment_inexpressible_shape_uses_pikevm_fallback() {
    assert_eq!(Glob::new("a\\/b*").unwrap().engine_name(), "PikeVm");
}

#[test]
fn classify_brace_routes_to_globstar_tier() {
    let g = Glob::new("*.{ts,tsx}").unwrap();
    // Brace pattern goes to Tier 2 because it needs the same machinery.
    assert_eq!(g.tier(), Tier::Globstar);
}

#[test]
fn classify_extglob() {
    // `!(foo)` = negate once, body = literal `(foo)` → Tier::Literal.
    let g = Glob::new("!(foo)").unwrap();
    assert_eq!(g.tier(), Tier::Literal);
    assert!(g.is_match(b"bar"));
    assert!(g.is_match(b"foo")); // 'foo' != '(foo)', negated, so matches
    assert!(!g.is_match(b"(foo)"));
}

#[test]
fn tier0_match_basic() {
    let g = Glob::new("src/main.rs").unwrap();
    assert!(g.is_match(b"src/main.rs"));
    assert!(!g.is_match(b"src/lib.rs"));
}

#[test]
fn tier0_negated() {
    let g = Glob::new("!foo").unwrap();
    assert_eq!(g.tier(), Tier::Literal);
    assert!(!g.is_match(b"foo"));
    assert!(g.is_match(b"bar"));
}

#[test]
fn tier0_double_negation() {
    let g = Glob::new("!!foo").unwrap();
    assert!(g.is_match(b"foo"));
    assert!(!g.is_match(b"bar"));
}

fn prefixes(pattern: &str) -> Vec<Vec<u8>> {
    Glob::new(pattern).unwrap().static_prefixes()
}

#[test]
fn prefixes_literal() {
    assert_eq!(prefixes("src/main.rs"), vec![b"src/main.rs".to_vec()]);
    assert_eq!(prefixes("foo"), vec![b"foo".to_vec()]);
}

#[test]
fn prefixes_simple_wildcard() {
    assert_eq!(prefixes("src/*.ts"), vec![b"src".to_vec()]);
    assert_eq!(prefixes("*.ts"), vec![b"".to_vec()]);
}

#[test]
fn prefixes_globstar() {
    assert_eq!(prefixes("src/**/*.ts"), vec![b"src".to_vec()]);
    assert_eq!(prefixes("**/*.ts"), vec![b"".to_vec()]);
    assert_eq!(prefixes("a/b/**/c"), vec![b"a/b".to_vec()]);
}

#[test]
fn prefixes_brace_multiple() {
    let p = prefixes("{src,tests}/*.rs");
    assert_eq!(p.len(), 2);
    assert!(p.contains(&b"src".to_vec()));
    assert!(p.contains(&b"tests".to_vec()));
}

#[test]
fn prefixes_brace_dedupes_nested() {
    // `{src,src/cli}/*.rs` — "src" subsumes "src/cli".
    let p = prefixes("{src,src/cli}/*.rs");
    assert_eq!(p, vec![b"src".to_vec()]);
}

#[test]
fn prefixes_empty_subsumes_all() {
    // `{**,src}/x` — one variant is `**/x` (prefix ""), the other is
    // `src/x` (prefix "src"). The empty prefix subsumes "src".
    let p = prefixes("{**,src}/x");
    assert_eq!(p, vec![b"".to_vec()]);
}

#[test]
fn prefixes_extglob_at_start() {
    // `@(foo|bar).ts` is fully literal — whole string is the prefix.
    assert_eq!(prefixes("@(foo|bar).ts"), vec![b"@(foo|bar).ts".to_vec()]);
}
