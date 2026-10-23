//! What large patterns keep, and peak at, while they compile: counted by
//! this binary's global allocator.

use std::sync::atomic::Ordering::Relaxed;

use globstar::{Glob, GlobError};

#[allow(dead_code)]
#[path = "../src/counter.rs"]
mod counter;
use counter::{LIVE, PEAK};

const MB: usize = 1 << 20;

/// (kept, peak) bytes of compiling `patterns` as one union, and the union.
fn cost(patterns: &[String]) -> (usize, usize, Glob) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let glob = Glob::union(patterns).unwrap();
    (LIVE.load(Relaxed) - base, PEAK.load(Relaxed) - base, glob)
}

// One test, so no other test thread allocates while it counts.
#[test]
fn large_patterns_stay_small() {
    let names = |n: usize, f: fn(usize) -> String| (0..n).map(f).collect::<Vec<_>>();
    let x = |n: usize| "x".repeat(n);
    let (x16, x32, nest) = (x(16_384), x(32_705), format!("{}/y", x(32_703)));
    let (one, distinct) = (format!("{x16}a"), format!("{x16}/aaa"));
    let cut = format!("abcdefghijklmnop/{x16}bq");
    let cases = [
        // A long name list stays a sorted suffix index.
        (
            "20,000 names",
            names(20_000, |i| format!("src/file{i}.ts")),
            "src/file14777.ts",
            true,
            MB,
            32 * MB,
        ),
        (
            "8,000 names in a brace",
            vec![format!(
                "{{{}}}",
                names(8_000, |i| format!("n{i}")).join(",")
            )],
            "n7999",
            true,
            MB,
            32 * MB,
        ),
        // A big union's graph NFA keeps no dense rows: its tables grow
        // with the members, not their square.
        (
            "20,000 dir/file members",
            names(20_000, |i| format!("dir{i}/file{i}")),
            "dir14777/file14777",
            true,
            16 * MB,
            32 * MB,
        ),
        (
            "20,000 ignore members",
            names(20_000, |i| {
                [
                    format!("**/node_modules{i}/**"),
                    format!("dist{i}/**"),
                    format!("**/*.gen{i}.ts"),
                    format!("pkg{i}/**/test/*.js"),
                ][i % 4]
                    .clone()
            }),
            "pkg7/x/test/a.js",
            true,
            16 * MB,
            32 * MB,
        ),
        // The chain index holds a key or so per byte: chains that share a
        // long list, or a brace of class branches, are not filed under
        // every string of it.
        (
            "64 heads, 12,000 names",
            vec![format!(
                "{{{}}}*{{{}}}",
                names(64, |i| format!("{i:x}/")).join(","),
                names(12_000, |i| format!("{i:x}")).join(",")
            )],
            "3f/a2edf",
            true,
            MB,
            8 * MB,
        ),
        (
            "11 heads, 1,000 class branches",
            vec![format!(
                "{{{}}}{{{}}}",
                names(11, |i| format!("{i:x}/")).join(","),
                names(1_000, |i| format!("[#-_]{i:x}")).join(",")
            )],
            "a/Z3e7",
            true,
            MB,
            8 * MB,
        ),
        // A long literal next to a product is not copied into every suffix.
        (
            "{a,b} x10 and 60 KB",
            vec![format!("*{}{}", "{a,b}".repeat(10), "z".repeat(60_000))],
            "az",
            false,
            4 * MB,
            16 * MB,
        ),
        // A long segment keeps no per-state closures past the budget.
        (
            "x*b*b...b, 64 KB",
            vec![format!("x{}b", "*b".repeat(32_000))],
            "xbb",
            false,
            8 * MB,
            32 * MB,
        ),
        // Braces that cross a `/` keep the walk to a few groups.
        (
            "{**,a}/ x256",
            vec!["{**,a}/".repeat(256) + "x"],
            "a/x",
            true,
            4 * MB,
            16 * MB,
        ),
        (
            "{a,b,/} x14",
            vec!["{a,b,/}".repeat(14)],
            "abababa/bababa",
            true,
            MB,
            16 * MB,
        ),
        (
            "{/,a,} x20",
            vec!["{/,a,}".repeat(20)],
            "aa/a",
            true,
            MB,
            16 * MB,
        ),
        (
            "{a/,} x800",
            vec!["{a/,}".repeat(800)],
            "a/a/",
            true,
            16 * MB,
            64 * MB,
        ),
        (
            "60,000 branches",
            vec![format!("x{{?{}}}", ",".repeat(60_000))],
            "xa",
            true,
            8 * MB,
            32 * MB,
        ),
        // A long head before many branches is not copied out once per
        // branch into the static prefixes: 32 KB patterns. Twin of the
        // check in packages/globstar/tests/large.mjs.
        (
            "16 KB head, 16,000 empty branches",
            vec![format!("{x16}{{{}}}", ",".repeat(16_382))],
            &x16,
            true,
            MB,
            16 * MB,
        ),
        (
            "32 KB head, {,,,,,,,} x7",
            vec![format!("{x32}{}", "{,,,,,,,}".repeat(7))],
            &x32,
            true,
            MB,
            16 * MB,
        ),
        (
            "32 KB head, {,,,,,,,} x7 and /y",
            vec![format!("{}/{}y", x(32_703), "{,,,,,,,}".repeat(7))],
            &nest,
            true,
            MB,
            16 * MB,
        ),
        (
            "16 KB head, 8,000 branches",
            vec![format!("{x16}{{{}a}}", "a,".repeat(8_191))],
            &one,
            true,
            MB,
            16 * MB,
        ),
        (
            "16 KB head, 4,096 names",
            vec![format!(
                "{x16}/{{{}}}",
                names(4_096, |i| format!("{i:x}")).join(",")
            )],
            &distinct,
            true,
            MB,
            16 * MB,
        ),
        (
            "16 KB head cut back by *, 8,000 branches",
            vec![format!(
                "abcdefghijklmnop/{x16}{{{}b}}*",
                "b,".repeat(8_191)
            )],
            &cut,
            true,
            MB,
            16 * MB,
        ),
    ];
    for (name, patterns, path, matches, max_kept, max_peak) in cases {
        let (kept, peak, glob) = cost(&patterns);
        assert!(kept <= max_kept, "{name}: keeps {kept} bytes");
        assert!(peak <= max_peak, "{name}: peaks at {peak} bytes");
        assert_eq!(glob.is_match(path.as_bytes()), matches, "{name}");
    }
    too_complex_patterns_fail_early();
    one_more_state_costs_little();
}

// One more state costs what the one before it did: the matcher keeps its
// tables at their length, never with the room a growing `Vec` leaves.
// Twin of the same check in packages/globstar/tests/large.mjs; each range
// stays in one NFA form, whose switch is a real step.
fn one_more_state_costs_little() {
    even_steps(
        "segment NFA",
        |k| format!("src/**/n*d[k-m]e?{}", "t".repeat(k)),
        2,
        20,
    );
    even_steps("graph NFA", |k| format!("**/a{}", "/**/a".repeat(k)), 1, 14);
}

fn even_steps(name: &str, shape: fn(usize) -> String, from: usize, to: usize) {
    let kept = |k: usize| cost(&[shape(k)]).0;
    let mut prev = kept(from + 1);
    let first = prev - kept(from);
    for k in from + 2..=to {
        let bytes = kept(k);
        let step = bytes - prev;
        assert!(
            step <= 2 * first,
            "{name}: {k} costs {step} bytes more than {}, the first step {first}",
            k - 1
        );
        prev = bytes;
    }
}

// Braces whose segments multiply would build a node per pair of them;
// the walk gives up early, with little memory spent.
fn too_complex_patterns_fail_early() {
    let lists = |n: usize| {
        let a: Vec<String> = (0..n).map(|i| format!("/a{i}")).collect();
        let b: Vec<String> = (0..n).map(|i| format!("b{i}/")).collect();
        format!("{{{}}}{{{}}}z", a.join(","), b.join(","))
    };
    for pattern in [
        format!("a{}b", "{a,/}".repeat(2048)),
        "{a,b,/}".repeat(4096),
        "{/,a,}".repeat(4096),
        lists(2048),
        "{**,a}/".repeat(8192) + "x",
        "{a/,}".repeat(8192),
        // Segments with big tables (suffix products, NFAs): charged
        // too, so neither shape keeps them up to the budget.
        "{a,b,/}".repeat(96),
        "{a,b,c,d,e,f,g,h,/}".repeat(3421),
    ] {
        let base = LIVE.load(Relaxed);
        PEAK.store(base, Relaxed);
        let err = Glob::new(&pattern).unwrap_err();
        assert!(matches!(err, GlobError::TooComplex { .. }), "{err:?}");
        let peak = PEAK.load(Relaxed) - base;
        assert!(peak <= 16 * MB, "{}: peaks at {peak} bytes", &pattern[..16]);
    }
}
