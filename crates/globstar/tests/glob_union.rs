//! Tests for `Glob::union` — the brace-merged multi-pattern compilation:
//!   - equivalence with matching the members one by one
//!     (`patterns.iter().any(|p| Glob::new(p).is_match(path))`)
//!   - edge cases: empty input, single-pattern degeneration, negated rejection
//!   - brace-meta safety: patterns containing `,` `{` `}` literal bytes
//!   - corpus-driven differential

use globstar::{CompileOptions, DirMatch, Glob, GlobError};
use std::collections::BTreeSet;
use std::fs;

/// Oracle: independently compile each pattern and OR the results.
/// Equivalent to what `Glob::union` should produce for non-negated
/// inputs, but computed without sharing any state across patterns.
fn or_oracle(patterns: &[&str], path: &[u8]) -> bool {
    patterns
        .iter()
        .any(|p| Glob::new(p).map(|g| g.is_match(path)).unwrap_or(false))
}

#[test]
fn empty_throws() {
    let result = Glob::union(std::iter::empty::<&str>());
    assert!(matches!(result, Err(GlobError::EmptyPatternSet)));
}

#[test]
fn members_share_one_expansion_budget() {
    // Each member is fine alone; ten of them would copy ten budgets' worth.
    let member = "{,/}".repeat(10) + "**/f";
    assert!(Glob::new(&member).is_ok());
    let members: Vec<String> = (0..10).map(|i| format!("{member}{i}")).collect();
    let err = Glob::union(&members).unwrap_err();
    // The member that ran the budget out is named.
    assert!(
        matches!(err, GlobError::InMember { index, .. } if index > 0),
        "{err:?}"
    );
    assert!(matches!(
        err.root(),
        GlobError::BraceExpansionTooLarge { .. }
    ));
}

#[test]
fn literal_paths_are_one_set() {
    // Members that are plain paths are kept as one sorted set, searched
    // once; a repeat folds; the others stay chains.
    let members = ["src/a.ts", "lib/b.ts", "src/a.ts", "a/b/c", "src/**/*.md"];
    let union = Glob::union(members).unwrap();
    assert_eq!(
        union.shape(),
        "L(4) G0 L(3) W:affix | chains 1 | literals 3 | nfa small"
    );
    for (path, want) in [
        ("src/a.ts", true),
        ("lib/b.ts", true),
        ("a/b/c", true),
        ("src/a.md", true),
        ("src/x/y.md", true),
        ("src/a.tsx", false),
        ("a/b", false),
        ("a/b/c/d", false),
        ("", false),
    ] {
        assert_eq!(union.is_match(path.as_bytes()), want, "{path}");
        assert_eq!(union.is_match_nfa(path.as_bytes()), want, "{path} (nfa)");
    }
    assert_eq!(union.match_dir(b"a/b"), DirMatch::Descend);
    assert_eq!(union.match_dir(b"a/b/c"), DirMatch::Match);
    assert_eq!(union.match_dir(b"lib"), DirMatch::Descend);
    assert_eq!(union.match_dir(b"x"), DirMatch::Pruned);
    // Case-insensitive: the set folds ASCII case on both sides.
    let folded = Glob::union_with(
        ["Src/A.ts", "src/a.ts", "LIB/b.ts", "x/*.md"],
        CompileOptions::default().case_insensitive(true),
    )
    .unwrap();
    assert!(folded.shape().contains("literals 2"));
    for (path, want) in [("SRC/a.TS", true), ("lib/B.TS", true), ("src/b.ts", false)] {
        assert_eq!(folded.is_match(path.as_bytes()), want, "{path}");
    }
    assert_eq!(
        or_oracle(&members, b"src/a.ts"),
        union.is_match(b"src/a.ts")
    );
}

#[test]
fn a_union_at_the_chain_cap_tries_every_chain() {
    // 64 wildcard chains and the literal set: one chain more than a word
    // has bits, so the chain index stands aside and the scan serves.
    let mut members: Vec<String> = (0..64).map(|i| format!("d{i}/**/*.x{i}")).collect();
    members.extend(["a/b.txt".into(), "c.txt".into()]);
    let union = Glob::union(&members).unwrap();
    assert!(union.shape().contains("| chains 64 | literals 2 |"));
    for i in 0..64 {
        let path = format!("d{i}/q/f.x{i}");
        assert!(union.is_match(path.as_bytes()), "{path}");
    }
    assert!(union.is_match(b"a/b.txt") && union.is_match(b"c.txt"));
    assert!(!union.is_match(b"d1/q/f.x2"));
}

#[test]
fn a_union_past_the_key_room_answers_as_its_members() {
    // One member gives more keys than the room (26 per class branch): its
    // chain is found by its head, the others by their keys.
    let classes: Vec<String> = (0..1000).map(|i| format!("[a-z]{i:x}")).collect();
    let mut members: Vec<String> = (0..20).map(|i| format!("d{i}/**/*.x{i}")).collect();
    members.push(format!("big/{{{}}}", classes.join(",")));
    // 64 chains share 2,000 names: more entries than the room, so the
    // union scans them.
    let heads: Vec<String> = (0..64).map(|i| format!("{i:x}/")).collect();
    let names: Vec<String> = (0..2000).map(|i| format!("n{i}")).collect();
    let shared = format!("{{{}}}*{{{}}}", heads.join(","), names.join(","));
    for (members, paths) in [
        (
            members.iter().map(String::as_str).collect::<Vec<_>>(),
            ["big/q3e7", "big/Q3e7", "big/q3e8", "d3/a/b.x3", "d3/b.x4"],
        ),
        (
            vec![shared.as_str()],
            ["3f/an1999", "3f/n2000", "40/n1", "a/n7", "3f/n19999"],
        ),
    ] {
        let union = Glob::union(&members).unwrap();
        assert!(union.is_match(paths[0].as_bytes()), "{}", paths[0]);
        for path in paths {
            let want = or_oracle(&members, path.as_bytes());
            assert_eq!(union.is_match(path.as_bytes()), want, "{path}");
        }
    }
}

#[test]
fn members_share_one_compile_work_bound() {
    // A list compiles as one pattern, so `TooComplex` is the list's:
    // not wrapped in `InMember`, even when one member alone is to blame.
    let heavy = "{a,b,/}".repeat(100);
    assert!(matches!(
        Glob::new(&heavy),
        Err(GlobError::TooComplex { .. })
    ));
    assert!(matches!(
        Glob::union(["a", heavy.as_str()]),
        Err(GlobError::TooComplex { .. })
    ));
    // Members that each compile alone can fail together.
    let member = "{a,b,/}".repeat(30);
    assert!(Glob::new(&member).is_ok());
    assert!(matches!(
        Glob::union([member.as_str(), &format!("x/{member}")]),
        Err(GlobError::TooComplex { .. })
    ));
}

#[test]
fn a_member_error_names_the_member() {
    use std::error::Error;
    let err = Glob::union(["src/**", "lib/**", "lib/[a"]).unwrap_err();
    let GlobError::InMember { index, error } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(*index, 2);
    assert_eq!(**error, GlobError::UnterminatedClass { at: 4 });
    assert_eq!(err.to_string(), format!("pattern 2: {error}"));
    assert!(err.source().is_some());
    // A union of one is still a list.
    assert!(matches!(
        Glob::union(["[z-a]"]),
        Err(GlobError::InMember { index: 0, .. })
    ));
    // A single pattern is not.
    assert!(matches!(
        Glob::new("[z-a]"),
        Err(GlobError::InvalidRange { .. })
    ));
}

#[test]
fn duplicate_members_count_once() {
    // A pattern given twice is parsed once and counts once: the union
    // stays literal, with the member's own prefix.
    let twice = Glob::union(["src/main.rs", "src/main.rs"]).unwrap();
    assert_eq!(twice.shape(), "literal");
    assert_eq!(
        twice.static_prefixes(),
        Glob::new("src/main.rs").unwrap().static_prefixes()
    );
    // The index in NegatedInUnion still counts every member given.
    assert!(matches!(
        Glob::union(["a", "a", "!b"]),
        Err(GlobError::NegatedInUnion { index: 2, .. })
    ));
}

#[test]
fn members_spelled_differently_fold_into_one() {
    // One body under two spellings: factoring lifts all of it and keeps
    // one empty branch, so nothing follows an open `**` (graph.rs
    // debug-asserts it) and a literal keeps the Literal engine.
    for (a, b) in [
        ("**", "**/**"),
        ("a/**", "a/**/**"),
        ("/**", "/**/**"),
        ("\\a", "a"),
    ] {
        let union = Glob::union([a, b]).unwrap();
        let single = Glob::new(b).unwrap();
        for path in ["", "a", "a/b", "/a", "/a/b", "x/y"] {
            let path = path.as_bytes();
            assert_eq!(union.is_match(path), single.is_match(path), "{a} {b}");
            assert_eq!(union.match_dir(path), single.match_dir(path), "{a} {b}");
        }
    }
    assert_eq!(Glob::union(["\\a", "a"]).unwrap().shape(), "literal");
}

#[test]
fn long_shared_prefixes_lift_in_one_pass() {
    // The shared tokens leave each member in one drain, not one per
    // token, so two 64 KB members factor in linear time.
    let (x, y) = ("a/".repeat(32_000) + "x", "a/".repeat(32_000) + "y");
    let start = std::time::Instant::now();
    let union = Glob::union([&x, &y]).unwrap();
    assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
    assert!(union.is_match(x.as_bytes()) && union.is_match(y.as_bytes()));
    assert!(!union.is_match(("a/".repeat(32_000) + "z").as_bytes()));
}

#[test]
fn single_pattern_degenerates() {
    let u = Glob::union(["foo.txt"]).unwrap();
    assert!(u.is_match(b"foo.txt"));
    assert!(!u.is_match(b"foo.tx"));
    assert_eq!(u.shape(), "literal");
}

#[test]
fn negated_members_are_rejected() {
    // Deliberate Rust/JS asymmetry: `Glob::union` is a pure OR of
    // positive patterns, so even a lone `!foo` is rejected — while the
    // JS `globstar()`/`compileMatcher()` factory accepts negation as
    // part of its include/exclude contract.
    for (members, index) in [(&["a", "!b"][..], 1), (&["!foo"][..], 0)] {
        assert!(
            matches!(
                Glob::union(members.iter().copied()),
                Err(GlobError::NegatedInUnion { index: i, .. }) if i == index
            ),
            "{members:?}"
        );
    }
}

#[test]
fn brace_meta_in_patterns_safe() {
    // Patterns with literal `,` `{` `}` must not be re-interpreted by the
    // AST-level merge — `union(["a,b.txt", "{x}.log"])` matches the
    // literal strings, NOT the brace-expansions of those bytes.
    let u = Glob::union(["a,b.txt", "{x}.log"]).unwrap();
    assert!(u.is_match(b"a,b.txt"));
    assert!(u.is_match(b"{x}.log"));
    assert!(!u.is_match(b"a"));
    assert!(!u.is_match(b"b.txt"));
    assert!(!u.is_match(b"x.log"));
}

#[test]
fn factoring_preserves_globstar_fold() {
    // Factoring must not lift the trailing `/` across a `**`: the union
    // of `//` and `a/**/` answers as the OR of its members.
    let patterns = ["//", "a/**/"];
    let u = Glob::union(patterns).unwrap();
    let test_paths: &[&[u8]] = &[b"a/", b"a/b/", b"//", b"a/b/c/", b"foo"];
    for p in test_paths {
        assert_eq!(u.is_match(p), or_oracle(&patterns, p), "path {p:?}");
    }
}

#[test]
fn members_that_differ_in_one_segment_match_as_the_members_do() {
    // Members apart only in the literal segment after a `**` run as one
    // chain with a set there; whatever the mix of roots, middles and
    // tails (and whether that segment is a literal in every member), the
    // union answers as the members do, under both dot settings.
    let roots = ["**", "src", "tests", "packages/*", "a/**"];
    let middles = ["node_modules", ".git", "dist", "x", "*.log", "*", "x/y"];
    let tails = ["/**", "", "/*.ts", "/x"];
    let paths: &[&[u8]] = &[
        b"",
        b"x",
        b"dist",
        b"a/dist/x",
        b"a/.git/x",
        b"a/.hidden/x",
        b".x/dist/y",
        b"src/node_modules/y.ts",
        b"src/x/y/x",
        b"src/x/y.ts",
        b"tests/x",
        b"packages/foo/dist/x.js",
        b"packages/dist/x.js",
        b"a/b/c/d.log",
        b"a/b/x/y/x",
    ];
    let mut seed = 0x2545_f491_u32;
    let mut next = |n: usize| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (seed >> 16) as usize % n
    };
    for _ in 0..300 {
        let size = 2 + next(5);
        let members: Vec<String> = (0..size)
            .map(|_| format!("{}/{}{}", roots[next(5)], middles[next(7)], tails[next(4)]))
            .collect();
        let refs: Vec<&str> = members.iter().map(String::as_str).collect();
        for dot in [true, false] {
            let opts = globstar::CompileOptions::default().dot(dot);
            let union = Glob::union_with(&refs, opts).unwrap();
            for path in paths {
                let want = refs
                    .iter()
                    .any(|p| Glob::new_with(p, opts).unwrap().is_match(path));
                assert_eq!(union.is_match(path), want, "{members:?} dot={dot} {path:?}");
            }
        }
    }
}

#[test]
fn factoring_lifts_shared_prefix_for_globstar_patterns() {
    // The whole point of factoring: the union produces the same segment
    // graph as the hand-written brace, so it matches identically.
    let u = Glob::union(["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.md"]).unwrap();
    let manual = Glob::new("**/*.{ts,tsx,js,jsx,md}").unwrap();
    let test_paths: &[&[u8]] = &[
        b"foo.ts",
        b"foo.tsx",
        b"foo.js",
        b"foo.jsx",
        b"foo.md",
        b"foo.rs",
        b"a/b/c/foo.ts",
        b"foo.txt",
        b"",
    ];
    for p in test_paths {
        assert_eq!(u.is_match(p), manual.is_match(p), "path {p:?}");
    }
}

#[test]
fn corpus_differential_vs_or_oracle() {
    // Load every (pattern, path) from the canonical corpus files and
    // verify `Glob::union(group).is_match` agrees with the inlined OR
    // oracle on a sampling of group sizes 2/3/5/10.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus");
    // Ordered sets, so each run tests the same groups and a failure
    // reproduces.
    let mut all_patterns: BTreeSet<String> = BTreeSet::new();
    let mut all_paths: BTreeSet<String> = BTreeSet::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".txt") || name.contains("err") {
            continue;
        }
        let text = fs::read_to_string(&p).unwrap();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 3 {
                continue;
            }
            let pattern = parts[0];
            let path = parts[1];
            // Skip negated (out of union scope) and any flag-bearing rows.
            if pattern.starts_with('!') {
                continue;
            }
            if parts.len() >= 4 && !parts[3].trim().is_empty() {
                continue;
            }
            all_patterns.insert(pattern.to_string());
            all_paths.insert(path.to_string());
        }
    }
    let valid: Vec<String> = all_patterns
        .into_iter()
        .filter(|p| Glob::new(p).is_ok())
        .collect();
    let paths: Vec<String> = all_paths.into_iter().take(50).collect();
    assert!(
        valid.len() >= 50,
        "corpus too small: {} patterns",
        valid.len()
    );

    // Deterministic "random" picker so test failures reproduce.
    let mut rng: u64 = 0xDEAD_BEEF;
    let mut next = || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (rng >> 32) as usize
    };
    let mut pick = |n: usize, src: &[String]| -> Vec<String> {
        let mut a: Vec<String> = src.to_vec();
        for i in (1..a.len()).rev() {
            let j = next() % (i + 1);
            a.swap(i, j);
        }
        a.truncate(n);
        a
    };

    let mut checks = 0usize;
    for &size in &[2usize, 3, 5, 10] {
        for _ in 0..50 {
            let group = pick(size, &valid);
            let u = match Glob::union(group.iter()) {
                Ok(g) => g,
                Err(_) => continue,
            };
            let group_refs: Vec<&str> = group.iter().map(|s| s.as_str()).collect();
            for path in &paths {
                let a = u.is_match(path.as_bytes());
                let b = or_oracle(&group_refs, path.as_bytes());
                assert_eq!(
                    a, b,
                    "size={size} group={group:?} path={path:?} u={a} oracle={b}"
                );
                checks += 1;
            }
        }
    }
    assert!(checks > 5_000, "too few checks: {checks}");
}

/// Members of a union too big for the chains: its graph NFA keeps dense
/// rows at 100 members and goes sparse at 600.
fn big_union(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            let (a, b) = (i % 37, i % 11);
            match i % 7 {
                0 => format!("a{a}/**"),
                1 => format!("**/f{a}.ts"),
                2 => format!("b{b}/a{a}/*"),
                3 => format!("{{a,b}}{a}/f{b}"),
                4 => format!("**/c{a}/**/d{b}"),
                5 => format!("f{a}/**/b{b}"),
                _ => format!("*{a}/a{b}"),
            }
        })
        .collect()
}

#[test]
fn big_unions_agree_with_their_members() {
    let segs = [
        "a3", "b2", "f5.ts", "c7", "d1", "x", "a30", ".a", "b10", "f36",
    ];
    let mut paths: Vec<String> = vec![String::new()];
    for depth in 1..=3 {
        for i in 0..200usize {
            let path: Vec<&str> = (0..depth)
                .map(|k| segs[(i / 10usize.pow(k)) % segs.len()])
                .collect();
            paths.push(path.join("/"));
        }
    }
    for n in [100, 600] {
        let patterns = big_union(n);
        let members: Vec<Glob> = patterns.iter().map(|p| Glob::new(p).unwrap()).collect();
        let union = Glob::union(&patterns).unwrap();
        for path in &paths {
            let path = path.as_bytes();
            let any = |f: &dyn Fn(&Glob) -> bool| members.iter().any(f);
            assert_eq!(
                union.is_match(path),
                any(&|g| g.is_match(path)),
                "{n}: {path:?}"
            );
            let dir = union.match_dir(path);
            assert_eq!(
                dir.matches(),
                any(&|g| g.match_dir(path).matches()),
                "{n}: {path:?}"
            );
            assert_eq!(
                dir.may_match_below(),
                any(&|g| g.match_dir(path).may_match_below()),
                "{n}: {path:?}"
            );
        }
    }
}
