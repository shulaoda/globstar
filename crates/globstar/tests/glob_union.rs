//! Tests for `Glob::union` — the brace-merged multi-pattern compilation.
//!
//! Mirrors the JS-side `verify_union.js` differential coverage:
//!   - basic equivalence with the inlined OR oracle
//!     (`patterns.iter().any(|p| Glob::new(p).is_match(path))`)
//!   - edge cases: empty input, single-pattern degeneration, negated rejection
//!   - brace-meta safety: patterns containing `,` `{` `}` literal bytes
//!   - corpus-driven differential

use globstar::{Glob, GlobError};
use std::collections::HashSet;
use std::fs;

/// Oracle: independently compile each pattern and OR the results.
/// Equivalent to what `Glob::union` should produce for non-negated
/// inputs, but computed without sharing any state across patterns.
fn or_oracle(patterns: &[&str], path: &[u8]) -> bool {
    patterns
        .iter()
        .any(|p| Glob::new(p).map(|g| g.is_match(path)).unwrap_or(false))
}

const PATHS: &[&[u8]] = &[
    b"foo.ts",
    b"foo.tsx",
    b"foo.js",
    b"foo.jsx",
    b"README.md",
    b"src/a/b.ts",
    b"src/main.rs",
    b"node_modules/foo/index.js",
];

#[test]
fn equivalence_with_or_oracle() {
    let patterns = ["**/*.ts", "**/*.tsx", "**/*.js"];
    let u = Glob::union(patterns).unwrap();
    assert_eq!(u.engine_name(), "Segment");
    for p in PATHS {
        assert_eq!(u.is_match(p), or_oracle(&patterns, p), "path {p:?}");
    }
}

#[test]
fn empty_throws() {
    let result = Glob::union(std::iter::empty::<&str>());
    assert!(matches!(result, Err(GlobError::EmptyPatternSet)));
}

#[test]
fn single_pattern_degenerates() {
    let u = Glob::union(["foo.txt"]).unwrap();
    assert!(u.is_match(b"foo.txt"));
    assert!(!u.is_match(b"foo.tx"));
    assert_eq!(u.engine_name(), "Literal");
}

#[test]
fn negated_pattern_rejected() {
    let result = Glob::union(["a", "!b"]);
    assert!(matches!(
        result,
        Err(GlobError::NegatedInUnion { index: 1, .. })
    ));
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
    // Regression: `["//", "a/**/"]` previously broke when the trailing
    // `/` was lifted across the `**` fold boundary.
    let patterns = ["//", "a/**/"];
    let u = Glob::union(patterns).unwrap();
    let test_paths: &[&[u8]] = &[b"a/", b"a/b/", b"//", b"a/b/c/", b"foo"];
    for p in test_paths {
        assert_eq!(u.is_match(p), or_oracle(&patterns, p), "path {p:?}");
    }
}

#[test]
fn factoring_lifts_shared_prefix_for_globstar_patterns() {
    // The whole point of factoring: union should produce the same SSM
    // as the hand-written brace, so this should match identically.
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
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut all_patterns: HashSet<String> = HashSet::new();
    let mut all_paths: HashSet<String> = HashSet::new();
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
