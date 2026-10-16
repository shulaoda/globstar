//! Multi-pattern memory comparison: globstar union (one merged matcher),
//! globstar per-pattern (N independent matchers), globset (one
//! GlobSet over Aho-Corasick + N regexes), wax (N independent
//! matchers). Mirrors the JS-side `matcher_multi.js`.
//!
//! Same counting-allocator trick as `matcher_single_memory.rs`: we measure
//! the net byte-count delta around each builder and its matches on the
//! bench paths. For per-pattern
//! modes we sum the allocations of all N matchers — that's the
//! actual memory cost a caller pays.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use globset::GlobBuilder;
use globstar::Glob;
use wax::Pattern as _;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

struct Counter;

unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Counter = Counter;

/// Returns net heap-byte delta around `f`. Inline size of the value
/// is intentionally NOT included here — multi-mode data depends on
/// how the caller wraps the matcher (`Vec<Glob>` vs one `globset::GlobSet`),
/// so we report heap-only and add the obvious wrapper inline cost
/// outside.
fn measure_heap<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATED.load(Ordering::SeqCst);
    let val = f();
    let after = ALLOCATED.load(Ordering::SeqCst);
    let heap = (after as isize - before as isize).max(0) as usize;
    (val, heap)
}

/// The 11 paths the match benches use: every matcher answers them all
/// before it is counted, since regex-backed ones build their search
/// cache on first use.
const PATHS: &[&str] = &[
    "src/main.ts",
    "src/cli/commands/build.ts",
    "src/cli/commands/run.rs",
    "src/main.rs",
    "tests/smoke.ts",
    "dist/bundle.js",
    "node_modules/foo/index.js",
    "package.json",
    ".env",
    "target/debug/build/foo",
    "src/a/bigger/path/to/the/crazy/needle.txt",
];

/// Median-of-9 to dampen allocator noise around the boundary alloc.
fn median(samples: &mut [usize]) -> usize {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn main() {
    let pattern_sets: &[(&str, &[&str])] = &[
        ("solo-globstar", &["**/*.ts"]),
        (
            "brace-equiv-4",
            &["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"],
        ),
        (
            "brace-equiv-8",
            &[
                "**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.mjs", "**/*.cjs", "**/*.mts",
                "**/*.cts",
            ],
        ),
        (
            "mixed-roots",
            &["src/**/*.ts", "tests/**/*.ts", "lib/**/*.js"],
        ),
        (
            "huge-set-pos",
            &[
                "src/**/*.ts",
                "src/**/*.tsx",
                "tests/**/*.test.ts",
                "lib/**/*.{js,mjs}",
                "**/*.json",
                "**/package.json",
                "**/.env*",
                "components/**/*.vue",
                "scripts/**/*.sh",
                "docs/**/*.md",
            ],
        ),
    ];

    fn build_globset(patterns: &[&str]) -> globset::GlobSet {
        let mut b = globset::GlobSetBuilder::new();
        for p in patterns {
            b.add(GlobBuilder::new(p).literal_separator(true).build().unwrap());
        }
        b.build().unwrap()
    }

    let trials = 9;

    println!(
        "{:<18} {:>3} {:>12} {:>12} {:>12} {:>12}",
        "Pattern set", "N", "gs_union", "gs_per", "globset", "wax_per"
    );
    println!(
        "{:-<18} {:->3} {:->12} {:->12} {:->12} {:->12}",
        "", "", "", "", "", ""
    );

    for &(label, patterns) in pattern_sets {
        let n = patterns.len();

        // globstar union — one merged Glob.
        let mut samples: Vec<usize> = (0..trials)
            .map(|_| {
                let (_v, heap) = measure_heap(|| {
                    let g = Glob::union(patterns.iter().copied()).unwrap();
                    for p in PATHS {
                        black_box(g.is_match(p.as_bytes()));
                    }
                    g
                });
                heap
            })
            .collect();
        let gs_union = median(&mut samples);

        // globstar per-pattern — N independent Glob matchers (heap of
        // a Vec<Glob> with N entries).
        let mut samples: Vec<usize> = (0..trials)
            .map(|_| {
                let (_v, heap) = measure_heap(|| {
                    let gs = patterns
                        .iter()
                        .map(|p| Glob::new(p).unwrap())
                        .collect::<Vec<_>>();
                    for p in PATHS {
                        black_box(gs.iter().any(|g| g.is_match(p.as_bytes())));
                    }
                    gs
                });
                heap
            })
            .collect();
        let gs_per = median(&mut samples);

        // globset — N regexes + Aho-Corasick prefilter, all merged.
        let mut samples: Vec<usize> = (0..trials)
            .map(|_| {
                let (_v, heap) = measure_heap(|| {
                    let g = build_globset(patterns);
                    for p in PATHS {
                        black_box(g.is_match(Path::new(p)));
                    }
                    g
                });
                heap
            })
            .collect();
        let globset_total = median(&mut samples);

        // wax per-pattern — N independent matchers.
        let mut samples: Vec<usize> = (0..trials)
            .map(|_| {
                let (_v, heap) = measure_heap(|| {
                    let gs = patterns
                        .iter()
                        .map(|p| wax::Glob::new(p).unwrap().into_owned())
                        .collect::<Vec<_>>();
                    for p in PATHS {
                        black_box(gs.iter().any(|g| g.is_match(*p)));
                    }
                    gs
                });
                heap
            })
            .collect();
        let wax_per = median(&mut samples);

        println!(
            "{:<18} {:>3} {:>12} {:>12} {:>12} {:>12}",
            label, n, gs_union, gs_per, globset_total, wax_per
        );
    }
}
