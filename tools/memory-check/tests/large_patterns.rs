//! What large patterns keep, and peak at, while they compile.

use std::sync::atomic::Ordering::Relaxed;

use globstar::Glob;

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
    let wide = copied(16, "[a-z", "\x01-\x7f", "]");
    let cases = [
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
        // A big union's graph NFA keeps no dense rows.
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
        // every string.
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
        // Braces that cross a `/` keep the walk to a few groups; side by
        // side, a few edges per brace.
        (
            "{**,a}/ x256",
            vec!["{**,a}/".repeat(256) + "x"],
            "a/x",
            true,
            4 * MB,
            16 * MB,
        ),
        (
            "{**,a}/ x8192",
            vec!["{**,a}/".repeat(8192) + "x"],
            "a/x",
            true,
            2 * MB,
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
            "{a/,} x64",
            vec!["{a/,}".repeat(64)],
            "a/a/",
            true,
            MB,
            16 * MB,
        ),
        (
            "{a/,} x8192",
            vec!["{a/,}".repeat(8192) + "x"],
            "a/a/x",
            true,
            2 * MB,
            16 * MB,
        ),
        (
            "60,000 branches",
            vec![format!("x{{?{}}}", ",".repeat(60_000))],
            "xa",
            true,
            8 * MB,
            32 * MB,
        ),
        (
            "16 KB class in 16 segments",
            vec![wide],
            "a/3x",
            true,
            MB,
            16 * MB,
        ),
        // A long head before many branches is not copied out once per
        // branch into the static prefixes.
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
    multiplying_segments_stay_linear();
    one_more_state_costs_little();
}

// The matcher keeps its tables at their length, never with the room a
// growing `Vec` leaves; each range stays in one NFA form.
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

/// `{a/0,…,a/(n-1)}`, then `open`, `unit` repeated and `close`: 16 KB
/// whose tail follows every one of the brace's `n` segments.
fn copied(n: usize, open: &str, unit: &str, close: &str) -> String {
    let heads: Vec<String> = (0..n).map(|i| format!("a/{i}")).collect();
    let head = format!("{{{}}}{open}", heads.join(","));
    let units = (16_384 - head.len() - close.len()) / unit.len();
    format!("{head}{}{close}", unit.repeat(units))
}

// Braces whose segments multiply copy segments until the copies weigh
// what the pattern does, then cut them into glued pieces: twice the
// pattern costs at most a little more than twice.
fn multiplying_segments_stay_linear() {
    let lists = |n: usize| {
        let a: Vec<String> = (0..n).map(|i| format!("/a{i}")).collect();
        let b: Vec<String> = (0..n).map(|i| format!("b{i}/")).collect();
        format!("{{{}}}{{{}}}z", a.join(","), b.join(","))
    };
    let shapes: [fn(usize) -> String; 13] = [
        |n| format!("a{}b", "{a,/}".repeat(n)),
        |n| "{a,b,/}".repeat(n),
        |n| "{/,a,}".repeat(n),
        |n| "{a,b,c,d,e,f,g,h,/}".repeat(n),
        |n| "{**,a}{/,b}".repeat(n),
        |n| "**{/a,b}".repeat(n),
        |n| "{a/**,b}".repeat(n),
        |n| "{x/,y}**".repeat(n),
        |n| "[a-z]{/,}".repeat(n),
        |n| "{a,}**".repeat(n),
        |n| format!("**{}", "{,/a}".repeat(n)),
        |n| "{a/,}".repeat(n),
        |n| "{**,a}/".repeat(n),
    ];
    for shape in shapes {
        let n = 16_384 / shape(1).len();
        let (small, _, _) = cost(&[shape(n / 2)]);
        let (kept, peak, _) = cost(&[shape(n)]);
        let name = &shape(1);
        assert!(
            kept <= 5 * small / 2,
            "{name}: keeps {small} bytes, then {kept}"
        );
        assert!(peak <= 8 * MB, "{name}: peaks at {peak} bytes");
    }
    // A long class, run of `?`s or classes, or brace of empty branches
    // after a brace that ends in many segments: copied onto a few, then
    // one piece the rest glue to.
    for pattern in [
        lists(2048),
        "{a,b,c,d,e,f,g,h,/}".repeat(3421),
        copied(500, "[a-z", "!-~", "]"),
        copied(1024, "", "?", ""),
        copied(512, "", "[\x01-\x7f]", ""),
        copied(1024, "{", ",", "}"),
    ] {
        let (kept, peak, _) = cost(std::slice::from_ref(&pattern));
        assert!(kept <= 8 * MB, "{}: keeps {kept} bytes", &pattern[..16]);
        assert!(peak <= 16 * MB, "{}: peaks at {peak} bytes", &pattern[..16]);
    }
}
