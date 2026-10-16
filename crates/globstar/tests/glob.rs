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
fn wide_graphs_step_by_what_is_active() {
    // 18,000 states, two of them active: a step touches what is active,
    // not every literal sibling group (100 matches took minutes).
    let g = Glob::new(&("{a/,b/}".repeat(9000) + "x")).unwrap();
    let path = "a/".repeat(100) + "x";
    let start = std::time::Instant::now();
    for _ in 0..100 {
        assert!(!g.is_match(path.as_bytes()));
    }
    assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
}

#[test]
fn match_dir_stays_cheap_on_long_wildcard_chains() {
    // The all-below proof feeds at most 64 wildcard-only segments, and
    // the chain's tail tokens are budgeted: each call took seconds.
    let union = Glob::union(["**".to_string(), format!("**/{}x", "*/".repeat(16_000))]).unwrap();
    let chain = Glob::new(&format!("**/{}*", "*/".repeat(32_000))).unwrap();
    let deep = "a/".repeat(32_000) + "a";
    let start = std::time::Instant::now();
    let shallow = union.match_dir(b"a");
    let long = chain.match_dir(deep.as_bytes());
    assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
    assert!(shallow.matches() && shallow.may_match_below());
    assert!(long.matches() && long.may_match_below());
}

#[test]
fn the_length_limit_is_exact() {
    // 65,536 bytes compile; one more is TooLong, in bytes, not chars.
    use globstar::error::MAX_PATTERN_LEN;
    assert!(Glob::new(&"a".repeat(MAX_PATTERN_LEN)).is_ok());
    assert_eq!(
        Glob::new(&"a".repeat(MAX_PATTERN_LEN + 1)).unwrap_err(),
        globstar::GlobError::TooLong {
            len: MAX_PATTERN_LEN + 1,
            max: MAX_PATTERN_LEN
        }
    );
    assert!(Glob::new(&"é".repeat(MAX_PATTERN_LEN / 2)).is_ok());
    assert!(Glob::new(&("é".repeat(MAX_PATTERN_LEN / 2) + "a")).is_err());
}

#[test]
fn errors_display_and_matchers_dispatch() {
    // Display is what a CLI prints; `&dyn Matcher` is how a walker holds
    // a matcher it did not build.
    let err = Glob::new("src/[a").unwrap_err();
    assert_eq!(
        err.to_string(),
        "unterminated character class at byte 4: no `]` before a `/` or the end of the pattern"
    );
    let m: &dyn globstar::Matcher = &Glob::new("src/**/*.ts").unwrap();
    assert!(m.is_match(b"src/a/b.ts"));
    assert!(!m.match_dir(b"lib").matches() && !m.match_dir(b"lib").may_match_below());
    assert_eq!(m.static_prefixes(), vec![b"src".to_vec()]);
}

#[test]
fn invalid_range_points_at_the_range() {
    // `at` is where the range starts, not where the class opens.
    let err = Glob::new("src/[abcz-a].ts").unwrap_err();
    assert_eq!(
        err,
        globstar::GlobError::InvalidRange {
            at: 8,
            low: b'z',
            high: b'a'
        }
    );
    assert!(err.to_string().contains("at byte 8"), "{err}");
}

#[test]
fn glob_debug_is_short() {
    // The compiled tables stay out of Debug (a union's ran to kilobytes).
    let names: Vec<String> = (0..50).map(|i| format!("src/m{i}/**/*.ts")).collect();
    let text = format!("{:?}", Glob::union(&names).unwrap());
    assert_eq!(
        text,
        r#"Glob { tier: Globstar, engine: "Segment", negated: false }"#
    );
}

#[test]
fn dir_match_new_combines_the_three_facts() {
    use globstar::DirMatch;
    assert_eq!(DirMatch::new(true, false, false), DirMatch::Match);
    assert_eq!(DirMatch::new(false, true, true), DirMatch::DescendAll);
    // `all_below` implies `may_below`.
    assert_eq!(
        DirMatch::new(true, false, true),
        DirMatch::DescendAllAndMatch
    );
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
fn prefixes_take_a_brace_with_what_follows_it() {
    // Each expansion's literal head: a brace in the head no longer cuts
    // the prefix at the brace.
    let sorted = |p: &str| {
        let mut v = prefixes(p);
        v.sort();
        v
    };
    assert_eq!(
        sorted("{node_modules,packages}/lodash/package.json"),
        vec![
            b"node_modules/lodash/package.json".to_vec(),
            b"packages/lodash/package.json".to_vec()
        ]
    );
    assert_eq!(
        sorted("{src,lib}/index.ts"),
        vec![b"lib/index.ts".to_vec(), b"src/index.ts".to_vec()]
    );
    assert_eq!(
        sorted("a{b,c}d/x"),
        vec![b"abd/x".to_vec(), b"acd/x".to_vec()]
    );
    assert_eq!(sorted("{a/**,b}/c"), vec![b"a".to_vec(), b"b/c".to_vec()]);
    assert_eq!(prefixes("src/{**,*.ts}"), vec![b"src".to_vec()]);
    // Doubling braces expand while the budget lasts (eight give 256
    // heads); sixteen pass it, and nothing whole is literal before them.
    assert_eq!(prefixes(&("{a,b}".repeat(8) + "/x")).len(), 256);
    assert_eq!(prefixes(&("{a,b}".repeat(16) + "/x")), vec![b"".to_vec()]);
}

#[test]
fn prefixes_stay_linear_in_the_pattern() {
    // A brace before a long literal copies it while the budget lasts,
    // then cuts the rest at their last whole segment.
    let branches: Vec<String> = (0..1000).map(|i| format!("b{i}")).collect();
    let pattern = format!("{{{}}}/{}", branches.join(","), "z".repeat(20_000));
    let p = prefixes(&pattern);
    assert_eq!(p.len(), 1000);
    let bytes: usize = p.iter().map(Vec::len).sum();
    assert!(bytes <= 4 * pattern.len(), "{bytes} bytes of prefixes");
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

// A class is a byte table: one lookup per byte, however many items it
// lists (it was a scan of every item for every byte: 30,000 items over
// 65,000 bytes took seconds). Twin of the check in large.mjs.
#[test]
fn a_long_class_costs_one_lookup_per_byte() {
    let glob = Glob::new(&format!("*[{}]", "b".repeat(30_000))).unwrap();
    let path = "a".repeat(65_000);
    let start = std::time::Instant::now();
    assert!(!glob.is_match(path.as_bytes()));
    assert!(glob.is_match(format!("{path}b").as_bytes()));
    assert!(start.elapsed().as_millis() < 300, "{:?}", start.elapsed());
}
