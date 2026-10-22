//! Integration tests for the Glob type.

use globstar::{CompileOptions, Glob};

#[test]
fn large_shapes_stay_on_segment_engine() {
    // A wide name list, a deep path and a brace product all compile and
    // match right on the segment engine.
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
    // About 3,500 in-segment states: past the closure budget, each step
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
    // not every literal sibling group.
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
    // the chain's tail tokens are budgeted.
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
    // The compiled tables stay out of Debug (a union's run to kilobytes).
    let names: Vec<String> = (0..50).map(|i| format!("src/m{i}/**/*.ts")).collect();
    let text = format!("{:?}", Glob::union(&names).unwrap());
    assert_eq!(text, r#"Glob { engine: "Segment", negated: false }"#);
}

/// The static prefixes of the rows of `fixtures/static-prefixes.tsv`,
/// shared with the JS tests: the patterns, `=>`, the sorted prefixes.
/// One pattern is `Glob::new`; more are a union.
#[test]
fn static_prefixes_match_the_shared_rows() {
    for line in include_str!("../../../fixtures/static-prefixes.tsv").lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let at = cols.iter().position(|c| *c == "=>").expect("a `=>` column");
        let (patterns, want) = (&cols[..at], &cols[at + 1..]);
        let glob = match patterns {
            [p] => Glob::new(p),
            ps => Glob::union(ps.iter().copied()),
        }
        .unwrap();
        let mut got: Vec<String> = glob
            .static_prefixes()
            .into_iter()
            .map(|p| String::from_utf8(p).unwrap())
            .collect();
        got.sort();
        assert_eq!(got, want, "{patterns:?}");
    }
}

#[test]
fn prefix_expansion_stays_within_the_budget() {
    let prefixes = |p: &str| Glob::new(p).unwrap().static_prefixes();
    // Doubling braces expand while the budget lasts (eight give 256
    // heads); sixteen pass it, and nothing whole is literal before them.
    assert_eq!(prefixes(&("{a,b}".repeat(8) + "/x")).len(), 256);
    assert_eq!(prefixes(&("{a,b}".repeat(16) + "/x")), vec![b"".to_vec()]);
    // A brace before a long literal copies it while the budget lasts,
    // then cuts the rest at their last whole segment.
    let branches: Vec<String> = (0..1000).map(|i| format!("b{i}")).collect();
    let pattern = format!("{{{}}}/{}", branches.join(","), "z".repeat(20_000));
    let p = prefixes(&pattern);
    assert_eq!(p.len(), 1000);
    let bytes: usize = p.iter().map(Vec::len).sum();
    assert!(bytes <= 4 * pattern.len(), "{bytes} bytes of prefixes");
}

// A class is a byte table: one lookup per byte, however many items it
// lists: 30,000 items over 65,000 bytes. Twin of the check in large.mjs.
#[test]
fn a_long_class_costs_one_lookup_per_byte() {
    let glob = Glob::new(&format!("*[{}]", "b".repeat(30_000))).unwrap();
    let path = "a".repeat(65_000);
    let start = std::time::Instant::now();
    assert!(!glob.is_match(path.as_bytes()));
    assert!(glob.is_match(format!("{path}b").as_bytes()));
    assert!(start.elapsed().as_millis() < 300, "{:?}", start.elapsed());
}

// The chain index reads a run of one-string nodes after a long list in
// one pass over the list: 6,000 names behind 10,000 such nodes. Twin of
// the check in large.mjs.
#[test]
fn a_run_after_a_long_list_reads_it_once() {
    let heads: Vec<String> = (0..11).map(|i| format!("p{i}/")).collect();
    let names: Vec<String> = (0..6000).map(|i| format!("{i:x}")).collect();
    let run = "[x]a".repeat(5000);
    let pattern = format!("{{{}}}{run}{{{}}}", heads.join(","), names.join(","));
    let start = std::time::Instant::now();
    let glob = Glob::new(&pattern).unwrap();
    assert!(start.elapsed().as_millis() < 300, "{:?}", start.elapsed());
    let path = format!("p3/{}176f", "xa".repeat(5000));
    assert!(glob.is_match(path.as_bytes()));
}

// Windows: an escaped `\` is a literal no segment holds (§4.2), so its
// expansion seeds nothing, and `match_dir` prunes where it is next.
#[cfg(windows)]
#[test]
fn an_escaped_backslash_is_dead_on_windows() {
    let union = Glob::union(["src\\\\sub/*.ts", "src/sub/*.ts"]).unwrap();
    assert_eq!(union.static_prefixes(), [b"src/sub".to_vec()]);
    assert!(
        Glob::new("src\\\\sub/*.ts")
            .unwrap()
            .static_prefixes()
            .is_empty()
    );
    let dead = Glob::new("a/b\\\\c/*.ts").unwrap();
    assert_eq!(dead.match_dir(b"a"), globstar::DirMatch::Pruned);
    // A pure literal too: the literal engine seeds nothing either.
    assert!(Glob::new("a\\\\b").unwrap().static_prefixes().is_empty());
}
