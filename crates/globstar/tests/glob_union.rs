//! `Glob::union` against matching the members one by one, its error
//! contract, and its limits.

use globstar::{CompileOptions, Glob, GlobError};
use std::collections::BTreeSet;
use std::fs;

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

// No budget is shared between members.
#[test]
fn members_that_compile_alone_compile_together() {
    let member = "{,/}".repeat(10) + "**/f";
    let members: Vec<String> = (0..10).map(|i| format!("{member}{i}")).collect();
    let refs: Vec<&str> = members.iter().map(String::as_str).collect();
    let union = Glob::union(&members).unwrap();
    for path in ["f0", "a/f3", "/f9", "a//b/f1", "///f2", "f", "a/f10"] {
        let want = or_oracle(&refs, path.as_bytes());
        assert_eq!(union.is_match(path.as_bytes()), want, "{path}");
    }
    assert!(union.is_match(b"a/f3") && !union.is_match(b"f"));
}

// One chain more than a word has bits: the index picks by id, not by bit.
#[test]
fn a_union_past_a_words_chains_names_each_by_id() {
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

// Past 64 chains the index alone picks them, while no call walks more
// than 64 by one key or without any, and their nodes stay within four
// times the pattern's weight; else the NFA serves.
#[test]
fn a_union_of_many_wildcard_members_keeps_its_chains_while_indexed() {
    let keyed: Vec<String> = (0..5000).map(|i| format!("d{i}/*/n{i}.js")).collect();
    let keyless: Vec<String> = (0..100).map(|i| format!("*{i}*/**")).collect();
    let crowded: Vec<String> = (0..100).map(|i| format!("d{i}/q{i}/n.js")).collect();
    let product = vec!["{a/x,b/y}/".repeat(30) + "*.x"];
    let (deep, shallow) = ("a/x/".repeat(30) + "f.x", "b/y/".repeat(29) + "f.x");
    for (members, chains) in [
        (&keyed, "| chains 5000 |"),
        (&keyless, "| chains 0 |"),
        (&crowded, "chains 0 | literals 100 |"),
        (&product, "| chains 0 |"),
    ] {
        let union = Glob::union(members).unwrap();
        assert!(union.shape().contains(chains), "{}", union.shape());
        let nfa = Glob::union_with(members, CompileOptions::default().force_nfa(true)).unwrap();
        for path in [
            "d7/x/n7.js",
            "d4999/q/n4999.js",
            "d7/x/n8.js",
            "d7/n7.js",
            "a17b/c",
            "x17/",
            "d7/q7/n.js",
            "d7/q8/n.js",
            &deep,
            &shallow,
        ] {
            let want = nfa.is_match(path.as_bytes());
            assert_eq!(union.is_match(path.as_bytes()), want, "{path}");
        }
    }
    let union = Glob::union(&keyed).unwrap();
    assert!(union.is_match(b"d4999/q/n4999.js") && !union.is_match(b"d7/x/n8.js"));
}

// Only the set's bytes are bounded, not how many paths it holds.
#[test]
fn a_union_of_many_file_names_keeps_its_literal_set() {
    let members: Vec<String> = (0..10_000)
        .map(|i| format!("src/m{}/f{i}.ts", i % 100))
        .collect();
    let union = Glob::union(&members).unwrap();
    assert!(
        union.shape().contains("| literals 10000 |"),
        "{}",
        union.shape()
    );
    assert!(union.is_match(b"src/m7/f9907.ts") && !union.is_match(b"src/m7/f9908.ts"));
}

#[test]
fn a_union_past_the_key_room_answers_as_its_members() {
    // One member gives more keys than the room: its chain is found by its
    // head, the others by their keys.
    let classes: Vec<String> = (0..1000).map(|i| format!("[a-z]{i:x}")).collect();
    let mut members: Vec<String> = (0..20).map(|i| format!("d{i}/**/*.x{i}")).collect();
    members.push(format!("big/{{{}}}", classes.join(",")));
    // 64 chains share 2,000 names: more entries than the room, so no index.
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

// `{a,b,/}` repeated cuts its segments into glued pieces.
#[test]
fn braces_whose_segments_multiply_compile_as_one() {
    let heavy = "{a,b,/}".repeat(100);
    let member = "{a,b,/}".repeat(28);
    let behind = format!("x/{member}");
    for members in [vec!["a", heavy.as_str()], vec![member.as_str(), &behind]] {
        let union = Glob::union(&members).unwrap();
        for path in ["a", "ab/ba", "x/a/b", "x//", "/a/b/", "abc", "x/ab"] {
            let want = or_oracle(&members, path.as_bytes());
            assert_eq!(union.is_match(path.as_bytes()), want, "{path}");
        }
    }
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
    // A union of one is still a list; a single pattern is not.
    assert!(matches!(
        Glob::union(["[z-a]"]),
        Err(GlobError::InMember { index: 0, .. })
    ));
    assert!(matches!(
        Glob::new("[z-a]"),
        Err(GlobError::InvalidRange { .. })
    ));
}

// A body given under two spellings matches as that body.
#[test]
fn members_spelled_differently_match_as_one() {
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
    let folded = Glob::union_with(
        ["Src/A.ts", "src/a.ts", "LIB/b.ts", "x/*.md"],
        CompileOptions::default().case_insensitive(true),
    )
    .unwrap();
    assert!(folded.shape().contains("literals 2"), "{}", folded.shape());
}

#[test]
fn long_shared_prefixes_build_in_linear_time() {
    let (x, y) = ("a/".repeat(32_000) + "x", "a/".repeat(32_000) + "y");
    let start = std::time::Instant::now();
    let union = Glob::union([&x, &y]).unwrap();
    assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
    assert!(union.is_match(x.as_bytes()) && union.is_match(y.as_bytes()));
    assert!(!union.is_match(("a/".repeat(32_000) + "z").as_bytes()));
}

// Members apart in one literal merge however deep they lie: 200 names under
// a 17-segment prefix are one set chain, not 200 chains.
#[test]
fn deep_members_apart_in_one_literal_merge() {
    let prefix: String = (0..17).map(|i| format!("p{i}/")).collect();
    let members: Vec<String> = (0..200).map(|i| format!("{prefix}n{i}/*.log")).collect();
    let union = Glob::union(&members).unwrap();
    assert!(union.shape().contains("| chains 1 |"), "{}", union.shape());
    assert!(union.is_match(format!("{prefix}n7/x.log").as_bytes()));
    assert!(!union.is_match(format!("{prefix}n200/x.log").as_bytes()));
}

// Unlike the JS factory, a union is a pure OR of positive patterns. The
// index counts every member given, a repeat too.
#[test]
fn negated_members_are_rejected() {
    for (members, index) in [
        (&["a", "!b"][..], 1),
        (&["!foo"][..], 0),
        (&["a", "a", "!b"][..], 2),
    ] {
        assert!(
            matches!(
                Glob::union(members.iter().copied()),
                Err(GlobError::NegatedInUnion { index: i, .. }) if i == index
            ),
            "{members:?}"
        );
    }
}

// Members apart only in the literal segment after a `**` run as one chain
// with a set there, whatever the mix of roots, middles and tails.
#[test]
fn members_that_differ_in_one_segment_match_as_the_members_do() {
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

// Groups of 2, 3, 5 and 10 corpus patterns against the OR of their
// members; the picks are deterministic, so a failure reproduces.
#[test]
fn corpus_differential_vs_or_oracle() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus");
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
            // Negated rows and rows with flags are out of scope.
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

/// Too big for the chains: dense NFA rows at 100 members, sparse at 600.
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
