//! Multi-pattern memory comparison: globstar union (one merged DFA),
//! globstar per-pattern (N independent matchers), globset (one
//! GlobSet over Aho-Corasick + N regexes), wax (N independent
//! matchers). Mirrors the JS-side `matcher_multi.js`.
//!
//! Same counting-allocator trick as `matcher_single_memory.rs`: we measure
//! the net byte-count delta around each builder. For per-pattern
//! modes we sum the allocations of all N matchers — that's the
//! actual memory cost a caller pays.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use globset::GlobBuilder;
use globstar::Glob as GcGlob;

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
/// how the caller wraps the matcher (Vec<Glob> vs single GlobSet),
/// so we report heap-only and add the obvious wrapper inline cost
/// outside.
fn measure_heap<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATED.load(Ordering::SeqCst);
    let val = f();
    let after = ALLOCATED.load(Ordering::SeqCst);
    let heap = (after as isize - before as isize).max(0) as usize;
    (val, heap)
}

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
                let (_v, heap) = measure_heap(|| GcGlob::union(patterns.iter().copied()).unwrap());
                heap
            })
            .collect();
        let gs_union = median(&mut samples);

        // globstar per-pattern — N independent Glob matchers (heap of
        // a Vec<Glob> with N entries).
        let mut samples: Vec<usize> = (0..trials)
            .map(|_| {
                let (_v, heap) = measure_heap(|| {
                    patterns
                        .iter()
                        .map(|p| GcGlob::new(p).unwrap())
                        .collect::<Vec<_>>()
                });
                heap
            })
            .collect();
        let gs_per = median(&mut samples);

        // globset — N regexes + Aho-Corasick prefilter, all merged.
        let mut samples: Vec<usize> = (0..trials)
            .map(|_| {
                let (_v, heap) = measure_heap(|| build_globset(patterns));
                heap
            })
            .collect();
        let globset_total = median(&mut samples);

        // wax per-pattern — N independent matchers.
        let mut samples: Vec<usize> = (0..trials)
            .map(|_| {
                let (_v, heap) = measure_heap(|| {
                    patterns
                        .iter()
                        .map(|p| wax::Glob::new(p).unwrap().into_owned())
                        .collect::<Vec<_>>()
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
