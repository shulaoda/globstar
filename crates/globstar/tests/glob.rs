//! Integration tests for the Glob type.

use globstar::{CompileOptions, Glob, Tier};

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
fn large_shapes_stay_on_segment_engine() {
    // A wide name list, a deep path and a brace product all compile to
    // the one segment engine; nothing falls back.
    let names: Vec<String> = (0..500).map(|i| format!("a{i}")).collect();
    let wide = format!("{{{}}}/**", names.join(","));
    let deep = (0..120)
        .map(|i| format!("d{i}"))
        .collect::<Vec<_>>()
        .join("/")
        + "/*.ts";
    let product = "{a,b}/".repeat(12) + "x";
    assert!(Glob::new(&wide).unwrap().is_match(b"a499/x/y"));
    assert!(!Glob::new(&wide).unwrap().is_match(b"a500/x/y"));
    assert!(
        Glob::new(&deep)
            .unwrap()
            .is_match(deep.replace("*.ts", "x.ts").as_bytes())
    );
    assert!(
        Glob::new(&product)
            .unwrap()
            .is_match(b"a/b/a/b/a/b/a/b/a/b/a/b/x")
    );
    assert!(
        !Glob::new(&product)
            .unwrap()
            .is_match(b"a/b/a/b/a/b/a/b/a/b/a/c/x")
    );
}

#[test]
fn long_segments_compile_on_a_small_stack() {
    // The in-segment closures are built without recursion, so a long
    // segment, or a long list under dot=false, fits a small stack.
    let compile = || {
        let seg = format!("a{}b", "*{,}".repeat(500));
        assert!(Glob::new(&seg).unwrap().is_match(b"axb"));
        let list: Vec<String> = (0..1000).map(|i| format!("*x{i}")).collect();
        let list = format!("{{{}}}", list.join(","));
        let g = Glob::new_with(&list, CompileOptions::default().dot(false)).unwrap();
        assert!(g.is_match(b"ax999") && !g.is_match(b".x999"));
    };
    let small = std::thread::Builder::new().stack_size(256 << 10);
    small.spawn(compile).unwrap().join().unwrap();
}

#[test]
fn huge_segments_match_without_kept_closures() {
    // About 4,200 in-segment states: past the closure budget, each step
    // closes its set as it goes.
    let list: Vec<String> = (0..600).map(|i| format!("*x{i}")).collect();
    let list = format!("{{{}}}", list.join(","));
    for (dot, path, want) in [
        (true, "abx123", true),
        (true, "x599", true),
        (true, "x600", false),
        (true, "ax5b", false),
        (true, ".x5", true),
        (false, ".x5", false),
        (false, "a.x5", true),
    ] {
        let g = Glob::new_with(&list, CompileOptions::default().dot(dot)).unwrap();
        assert_eq!(g.is_match(path.as_bytes()), want, "{path} dot={dot}");
    }
}

#[test]
fn classify_brace_routes_to_globstar_tier() {
    let g = Glob::new("*.{ts,tsx}").unwrap();
    // A brace counts as `Tier::Globstar`; the tier is syntax only.
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
fn prefixes_trailing_slash_globstar() {
    // Strict trailing `/**` keeps the full literal head as the seed.
    assert_eq!(prefixes("src/**"), vec![b"src".to_vec()]);
    assert_eq!(prefixes("a/b/**"), vec![b"a/b".to_vec()]);
    let p = prefixes("{src,tests}/**");
    assert_eq!(p.len(), 2);
    assert!(p.contains(&b"src".to_vec()));
    assert!(p.contains(&b"tests".to_vec()));
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
