//! Four times the input costs about four times as much: theory note 03,
//! §11. A pattern four times as long may take at most five times the
//! memory and eight times the time to compile, and a call at most eight
//! times the time; a quadratic cost would take sixteen.

use std::hint::black_box;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use globstar::Glob;

#[allow(dead_code)]
#[path = "../src/counter.rs"]
mod counter;
use counter::{LIVE, PEAK};

/// The patterns at size `k`, and a path for the calls.
type Shape = fn(usize) -> (Vec<String>, String);

fn one(pattern: String, path: String) -> (Vec<String>, String) {
    (vec![pattern], path)
}

fn list(n: usize, f: impl Fn(usize) -> String, sep: &str) -> String {
    members(n, f).join(sep)
}

fn members(n: usize, f: impl Fn(usize) -> String) -> Vec<String> {
    (0..n).map(f).collect()
}

/// Compiled at `k` and `4k`, each about 16 and 64 KiB.
const BUILDS: [(&str, usize, Shape); 12] = [
    ("{a/,} x k", 3_200, |k| {
        one("{a/,}".repeat(k) + "x", "a/x".into())
    }),
    ("{**,a}/ x k", 2_300, |k| {
        one("{**,a}/".repeat(k) + "x", "a/x".into())
    }),
    ("x{a,/} x k", 2_700, |k| {
        one("x{a,/}".repeat(k), "xa".into())
    }),
    ("{a*,b*} x k", 2_300, |k| {
        one("{a*,b*}".repeat(k) + "c", "ac".into())
    }),
    ("*a x k", 8_000, |k| one("*a".repeat(k) + "b", "ab".into())),
    // The static prefixes pay for each look at a brace's branches.
    ("{a,b} x 64, then k branches", 5_300, |k| {
        let p = format!("{}{{{}/x}}", "{a,b}".repeat(64), "/x,".repeat(k));
        one(p, "a/x".into())
    }),
    ("{,} x 63, k empty, k/2 crossing", 5_300, |k| {
        let p = format!(
            "x{}{{{}}}{{{}/}}y",
            "{,}".repeat(63),
            ",".repeat(k),
            "/,".repeat(k / 2)
        );
        one(p, "x/y".into())
    }),
    // Chains that share a long element read its keys once.
    ("k crossing branches, one long tail", 900, |k| {
        let p = format!(
            "{{{}}}/{}",
            list(k, |i| format!("a{i}/*"), ","),
            "x".repeat(4 * k)
        );
        one(p, "a1/b/x".into())
    }),
    ("k heads under heads", 235, |k| {
        let heads = list(k, |i| format!("{}y", "x/".repeat(i % 64 + 1)), ",");
        one(format!("{{{heads}}}/*"), "x/y/z".into())
    }),
    ("union of k globstar blocks", 1_300, |k| {
        (members(k, |i| format!("**/x{i}/**")), "q/x7/z".into())
    }),
    ("union of k pieces", 1_300, |k| {
        (members(k, |i| format!("*{i}?*{i}")), "717".into())
    }),
    ("union of k merged names", 1_000, |k| {
        (members(k, |i| format!("src/f{i}.ts")), "src/f7.ts".into())
    }),
];

/// Called on a fixed path, the pattern at `k` and `4k`.
const CALLS: [(&str, usize, Shape); 5] = [
    // A costly wildcard many positions lead to is tested once a step.
    ("k branches into one wide wildcard", 1_000, |k| {
        let p = format!("{{{}*/}}{}c", "*/,".repeat(k - 1), "{*,a}".repeat(k));
        one(p, format!("x/{}c", "b".repeat(64)))
    }),
    ("k blocks into one wide wildcard", 250, |k| {
        let blocks = list(k, |i| format!("**/a{i}/**/"), ",");
        let p = format!("{{{blocks}}}{}q{{*,a}}c", "{*,a}".repeat(k));
        one(
            p,
            format!("{}{}c", list(64, |i| format!("a{i}/"), ""), "b".repeat(64)),
        )
    }),
    ("{**,a}/ x k", 2_300, |k| {
        one("{**,a}/".repeat(k) + "x", format!("{}x", "a/".repeat(64)))
    }),
    ("{a*,b*} x k", 2_300, |k| {
        one("{a*,b*}".repeat(k) + "c", format!("{}cc", "ab".repeat(64)))
    }),
    ("union of k pieces", 1_300, |k| {
        (
            members(k, |i| format!("**/*{i}*/**")),
            format!("{}z", "q/".repeat(64)),
        )
    }),
];

/// Called on a path of `k` and `4k` segments, more than the pattern's.
const PATHS: [(&str, usize, Shape); 2] = [
    ("**/a/ x 64, k segments", 256, |k| {
        one("**/a/".repeat(64) + "b", format!("{}c", "a/".repeat(k)))
    }),
    ("{**,a}/ x 64, k segments", 256, |k| {
        one("{**,a}/".repeat(64) + "x", format!("{}x", "a/".repeat(k)))
    }),
];

// One test, so no other test thread allocates while it counts.
#[test]
fn four_times_the_input_costs_about_four_times() {
    for (name, k, shape) in BUILDS {
        let (small, big) = (shape(k).0, shape(4 * k).0);
        let ((kept, peak), (kept4, peak4)) = (memory(&small), memory(&big));
        assert!(
            kept4 <= 5 * kept,
            "{name}: keeps {kept} bytes, then {kept4}"
        );
        assert!(
            peak4 <= 5 * peak,
            "{name}: peaks at {peak} bytes, then {peak4}"
        );
        let (t, t4) = timed(|| build(&small), || build(&big));
        assert!(t4 <= 8 * t, "{name}: compiles in {t:?}, then {t4:?}");
    }
    for (name, k, shape) in CALLS {
        let ((small, path), (big, _)) = (shape(k), shape(4 * k));
        let (glob, glob4) = (build(&small), build(&big));
        let (t, t4) = timed(|| calls(&glob, &path), || calls(&glob4, &path));
        assert!(t4 <= 8 * t, "{name}: a call takes {t:?}, then {t4:?}");
    }
    for (name, k, shape) in PATHS {
        let ((patterns, path), (_, path4)) = (shape(k), shape(4 * k));
        let glob = build(&patterns);
        let (t, t4) = timed(|| calls(&glob, &path), || calls(&glob, &path4));
        assert!(t4 <= 8 * t, "{name}: a call takes {t:?}, then {t4:?}");
    }
}

fn build(patterns: &[String]) -> Glob {
    Glob::union(patterns).unwrap()
}

/// What compiling keeps, and the most it holds at once.
fn memory(patterns: &[String]) -> (usize, usize) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let glob = build(patterns);
    let out = (LIVE.load(Relaxed) - base, PEAK.load(Relaxed) - base);
    drop(glob);
    out
}

fn calls(glob: &Glob, path: &str) {
    black_box(glob.is_match(black_box(path.as_bytes())));
    black_box(glob.match_dir(black_box(path.as_bytes())));
}

/// The least time each of `a` and `b` takes, in turns.
fn timed<A, B>(mut a: impl FnMut() -> A, mut b: impl FnMut() -> B) -> (Duration, Duration) {
    let (mut ta, mut tb) = (Duration::MAX, Duration::MAX);
    for _ in 0..3 {
        ta = ta.min(once(&mut a));
        tb = tb.min(once(&mut b));
    }
    (ta, tb)
}

/// One run's time, the run repeated until it takes 2 ms.
fn once<T>(f: &mut impl FnMut() -> T) -> Duration {
    let start = Instant::now();
    let mut n = 0;
    while n == 0 || start.elapsed() < Duration::from_millis(2) {
        black_box(f());
        n += 1;
    }
    start.elapsed() / n
}
