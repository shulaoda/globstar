//! What large patterns keep, and peak at, while they compile: counted by
//! this binary's global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use globstar::Glob;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const MB: usize = 1 << 20;

/// (kept, peak) bytes of compiling `patterns` as one union.
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
            "{a/,} x1500",
            vec!["{a/,}".repeat(1500)],
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
    ];
    for (name, patterns, path, matches, max_kept, max_peak) in cases {
        let (kept, peak, glob) = cost(&patterns);
        assert!(kept <= max_kept, "{name}: keeps {kept} bytes");
        assert!(peak <= max_peak, "{name}: peaks at {peak} bytes");
        assert_eq!(glob.is_match(path.as_bytes()), matches, "{name}");
    }
}
