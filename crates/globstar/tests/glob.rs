//! Integration tests for the Glob type.

use globstar::{CompileOptions, Glob, GlobError};

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

// Braces with a `**/` side by side chain their globstars, so the graph
// keeps a few edges per brace: 8,192 of them (57 KB) compile, and a path
// pays per segment for the states, not their square. Twin of the check
// in large.mjs.
#[test]
fn side_by_side_globstar_braces_stay_linear() {
    let path = "a/".repeat(16) + "x";
    for unit in ["{**,a}/", "{**,*}/"] {
        let start = std::time::Instant::now();
        let g = Glob::new(&(unit.repeat(8192) + "x")).unwrap();
        assert!(g.is_match(path.as_bytes()), "{unit}");
        assert!(g.match_dir(path.as_bytes()).matches(), "{unit}");
        let took = start.elapsed();
        assert!(took.as_secs() < 2, "{unit}: {took:?}");
    }
}

// Branches that may be empty, side by side, let each segment follow
// every one before it. The ones before it share a join node, so the graph
// keeps an edge or two per unit of pattern: it compiles fast, and no
// segment walks a successor list as long as the pattern. Twin of the
// check in large.mjs.
#[test]
fn edges_that_multiply_stay_linear() {
    for (pattern, path) in [
        ("{,/**/a}".repeat(800) + "/x", "/a".repeat(2000) + "/x"),
        ("{a/,}".repeat(800) + "x", "a/".repeat(800) + "x"),
    ] {
        let start = std::time::Instant::now();
        let g = Glob::new(&pattern).unwrap();
        assert!(start.elapsed().as_secs() < 1, "{:?}", start.elapsed());
        let start = std::time::Instant::now();
        assert!(g.is_match(path.as_bytes()), "{}", &pattern[..8]);
        let dir = &path[..path.rfind('/').unwrap()];
        assert!(
            g.match_dir(dir.as_bytes()).may_match_below(),
            "{}",
            &pattern[..8]
        );
        assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
    }
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
fn branches_that_split_names_by_length_cover_together() {
    // `?` takes every one-byte name and `??*` every longer one: side by
    // side they cover every name, though neither does alone, in every
    // NFA kind (padding grows the graph past each kind's size) and the
    // JS twin alike (GLOB_SPEC §15.5).
    let dot = CompileOptions::default().dot(true);
    let below = |p: &str, dir: &str| {
        let g = Glob::new_with(p, dot).unwrap();
        g.match_dir(dir.as_bytes()).matches_all_below()
    };
    for pad in [0, 20, 60, 200] {
        let pads: String = (0..pad).map(|i| format!(",p{i}/x")).collect();
        assert!(below(&format!("{{**/?,**/??*{pads}}}"), ""), "{pad}");
        assert!(below(&format!("{{**/?,**/??*{pads}}}"), "a/bc"), "{pad}");
        assert!(below(&format!("{{?/**/?,**/??*{pads}}}"), "a"), "{pad}");
        // No branch takes two bytes.
        assert!(!below(&format!("{{**/?,**/???*{pads}}}"), "a"), "{pad}");
    }
    // Lengths are told apart up to 31 bytes, every longer one as one.
    let q = |n: usize| "?".repeat(n);
    let upto = |k: usize, skip: usize| -> String {
        (1..=k)
            .filter(|&n| n != skip)
            .map(|n| format!("**/{},", q(n)))
            .collect()
    };
    assert!(below(&format!("{{{}**/{}*}}", upto(31, 0), q(32)), ""));
    assert!(!below(&format!("{{{}**/{}*}}", upto(31, 17), q(32)), ""));
    assert!(!below(&format!("{{{}**/{}*}}", upto(30, 0), q(32)), ""));
    // A run of 33 is past the classes: not provable, though it covers.
    assert!(!below(
        &format!("{{{}**/{},**/{}*}}", upto(31, 0), q(32), q(33)),
        ""
    ));
}

#[test]
fn the_all_below_search_by_length_stays_cheap() {
    // A covering pair beside many `?` runs, so that every length class
    // fires sets of its own: the search gives up within its caps.
    let mut seed = 7u32;
    let mut rnd = |n: u32| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7fff_ffff;
        seed % n
    };
    let mut branches = vec!["**/?".to_string(), "**/??*".to_string()];
    for i in 0..400 {
        let depth = 1 + rnd(6);
        let segs: Vec<String> = (0..depth)
            .map(|_| "?".repeat(1 + rnd(31) as usize))
            .collect();
        branches.push(format!("**/{}/z{i}", segs.join("/")));
    }
    let g = Glob::new_with(
        &format!("{{{}}}", branches.join(",")),
        CompileOptions::default().dot(true),
    )
    .unwrap();
    let start = std::time::Instant::now();
    for _ in 0..200 {
        for dir in ["", "a", "ab/c", "abcdefgh/x/yy"] {
            assert!(g.match_dir(dir.as_bytes()).may_match_below());
        }
    }
    assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
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
    // A head costs the bytes read to reach it: a file list keeps its heads,
    // while 4,096 names behind a 16 KB folder pass 64 budgets of those and
    // keep the folder.
    let files: Vec<String> = (0..4000)
        .map(|i| format!("packages/foo/src/components/xyz/Comp{i:04}.tsx"))
        .collect();
    assert_eq!(Glob::union(&files).unwrap().static_prefixes().len(), 4000);
    let names: Vec<String> = (0..4096).map(|i| format!("{i:x}")).collect();
    let folder = "x".repeat(16_384);
    let p = prefixes(&format!("{folder}/{{{}}}", names.join(",")));
    assert!(p == [folder.as_bytes()], "{} prefixes", p.len());
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

// Two globstars around a block: under `dot=false` the second one's dot
// check reads the middle once, not once per place the block is tried.
// Twin of the check in large.mjs.
#[test]
fn a_block_between_globstars_reads_the_middle_once() {
    let glob = Glob::new_with("**/x/**/y", CompileOptions::default().dot(false)).unwrap();
    let middle = "x/".repeat(32_000);
    let start = std::time::Instant::now();
    assert!(!glob.is_match(format!("{middle}.h/y").as_bytes()));
    assert!(glob.is_match(format!("{middle}y").as_bytes()));
    assert!(start.elapsed().as_millis() < 300, "{:?}", start.elapsed());
}

// A union's merge reads each member at each of its segments: long members
// alike in every segment yet never the same are left apart, not read once
// per segment, so 64 KB of them compiles in milliseconds. Twin of the
// check in large.mjs.
#[test]
fn a_union_of_long_members_alike_in_every_segment_compiles_fast() {
    let middle = "a/".repeat(16_000);
    let few = vec![
        format!("*/{middle}x/p"),
        format!("*/{middle}y/q"),
        "z".into(),
    ];
    let many: Vec<String> = (0..16)
        .map(|i| format!("*/m{i:02}/{}n{i:02}", "a/".repeat(2000)))
        .chain(["Q".into()])
        .collect();
    for (members, hit, miss) in [
        (few, format!("b/{middle}x/p"), format!("b/{middle}x/q")),
        (
            many,
            format!("b/m03/{}n03", "a/".repeat(2000)),
            "b/m03/n04".into(),
        ),
    ] {
        let start = std::time::Instant::now();
        let union = Glob::union(&members).unwrap();
        assert!(start.elapsed().as_millis() < 300, "{:?}", start.elapsed());
        assert!(union.is_match(hit.as_bytes()) && !union.is_match(miss.as_bytes()));
    }
}

// The graph walk finds a union's groups by all they hold: members alike
// but for a class or an in-segment brace are not each compared with
// every one before, so 32,768 of them compile in milliseconds. Twin of
// the check in large.mjs.
#[test]
fn members_apart_only_in_a_class_or_brace_compile_fast() {
    let side = |i: usize| ["a", "b"][i % 2];
    let list = |form: &dyn Fn(usize) -> String| -> Vec<String> {
        (0..32_768).map(form).chain(["c/d".into()]).collect()
    };
    for (members, hit) in [
        (list(&|i| format!("{}/[{i:04x}]x", side(i))), "b/3x"),
        (list(&|i| format!("{}/{{{i:04x},q}}x", side(i))), "b/qx"),
        (list(&|i| format!("**/[{i:04x}].log")), "x/3.log"),
    ] {
        let start = std::time::Instant::now();
        let union = Glob::union(&members).unwrap();
        let elapsed = start.elapsed();
        assert!(elapsed.as_millis() < 1000, "{hit}: {elapsed:?}");
        assert!(union.is_match(hit.as_bytes()) && !union.is_match(b"b/zzx"));
    }
}

// Keying groups by all they hold costs the walk their weight, where an
// empty brace branch weighs one: groups that each hold a brace of 40,000
// empty branches fail in milliseconds, in a list past the budget or met
// again at 30 nested braces. Twin of the check in large.mjs.
#[test]
fn groups_keyed_whole_count_toward_the_compile_work() {
    let core = |n: usize, m: usize| {
        let classes: Vec<String> = (0..n)
            .map(|i| {
                let (a, b) = (b'a' + (i / 26) as u8, b'A' + (i % 26) as u8);
                format!("/[{}{}]", a as char, b as char)
            })
            .collect();
        format!("{{{}}}{{{}}}", classes.join(","), ",".repeat(m))
    };
    let nested = (0..30).fold(core(60, 40_000), |p, i| format!("{{{p},/z{i}}}"));
    for members in [
        vec![
            format!("p{}{{y,z/}}", core(650, 40_000)),
            format!("q{}{{y,z/}}", core(650, 40_000)),
        ],
        vec![nested + "{y,/q}"],
    ] {
        let start = std::time::Instant::now();
        let err = Glob::union(&members).unwrap_err();
        let took = start.elapsed();
        assert!(matches!(err, GlobError::TooComplex { .. }), "{err:?}");
        assert!(
            took.as_millis() < 500,
            "{} members: {took:?}",
            members.len()
        );
    }
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

// The chain index searches a path's last segment for the first 16 bytes
// of a member's literal at most, and the walk checks the rest: members
// `x3/*` + 400 `a`s + `q3*` turn down a 64 KB segment of `a`s in
// microseconds, without a compare of the whole literal at every byte.
// Twin of the check in large.mjs.
#[test]
fn a_long_infix_is_searched_for_its_start() {
    let lit = "a".repeat(400);
    let members: Vec<String> = (0..16).map(|i| format!("x{i}/*{lit}q{i}*")).collect();
    let far = format!("d/{}", "a".repeat(65_536));
    for ci in [false, true] {
        let opts = CompileOptions::default().case_insensitive(ci);
        let union = Glob::union_with(&members, opts).unwrap();
        let start = std::time::Instant::now();
        assert!(!union.is_match(far.as_bytes()));
        let took = start.elapsed();
        assert!(took.as_millis() < 500, "ci={ci}: {took:?}");
        assert!(union.is_match(format!("x3/{lit}q3").as_bytes()));
        assert!(!union.is_match(format!("x3/{}q3", &lit[1..]).as_bytes()));
    }
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
