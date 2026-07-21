//! Cross-runtime memory comparison: forces each Rust globstar matcher
//! down a specific engine (DFA / PikeVM) on the same 7 patterns the JS
//! `matcher_single.js` Phase-3 table uses, so the rows line up
//! one-for-one with the JS column.
//!
//! Counting `GlobalAlloc` wrapper tracks net bytes-allocated, and we
//! measure (after − before) right around each engine's `build`. Inline
//! + heap = total per matcher.

use std::alloc::{GlobalAlloc, Layout, System};
use std::mem::size_of_val;
use std::sync::atomic::{AtomicUsize, Ordering};

use globset::GlobBuilder;
use globstar::engine::ops::lower;
use globstar::engine::pikevm::PikeVm;
use globstar::engine::thompson_dfa::ThompsonDfa;
use globstar::{CompileOptions, Glob, parser};

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

/// Build a `ThompsonDfa` for `pattern`. Mirrors the bench's `build_dfa`
/// — `dot=true` matches the JS default (`DEFAULT_OPTIONS.dot = true` in
/// `glob.js`) so heap counts compare across runtimes on equal terms.
fn build_dfa(pattern: &str) -> Box<ThompsonDfa> {
    let ast = parser::parse(pattern.as_bytes()).expect("parse");
    let program = lower(&ast.body, false);
    ThompsonDfa::build(program, true).unwrap_or_else(|_| panic!("DFA cap on `{pattern}`"))
}

fn build_pikevm(pattern: &str) -> PikeVm {
    let ast = parser::parse(pattern.as_bytes()).expect("parse");
    let program = lower(&ast.body, false);
    PikeVm::new(program, true)
}

/// Public-API build of the existing crate. `dot=true` mirrors the
/// JS default.
fn build_public(pattern: &str) -> Glob {
    Glob::new_with(pattern, CompileOptions::default().dot(true)).expect("compile")
}

/// Public-API build of the experimental `globstar-segment` crate.
fn build_segment(pattern: &str) -> globstar_segment::SegGlob {
    globstar_segment::SegGlob::new_with(pattern, CompileOptions::default().dot(true))
        .expect("compile")
}

/// Returns `(value, inline_size, net_heap_bytes)`.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    let before = ALLOCATED.load(Ordering::SeqCst);
    let val = f();
    let after = ALLOCATED.load(Ordering::SeqCst);
    let inline = size_of_val(&val);
    let heap = (after as isize - before as isize).max(0) as usize;
    (val, inline, heap)
}

/// Median-of-N: build N matchers in a row, take the median per-matcher
/// total. Mirrors the JS bench's median-of-5 trial loop, except in
/// Rust we know the exact per-call delta so we don't need to amortize
/// noise across 1000 builds.
fn median_total(n: usize, mut build: impl FnMut() -> usize) -> usize {
    let mut samples: Vec<usize> = (0..n).map(|_| build()).collect();
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn main() {
    // Same 7 patterns as packages/bench/benches/matcher_single.js.
    let patterns: &[(&str, &str)] = &[
        ("literal", "src/main.rs"),
        ("simple-wildcard", "src/*.ts"),
        ("globstar", "src/**/*.ts"),
        ("brace-suffix", "**/*.{ts,tsx,js,jsx}"),
        ("reject-prefilter", "**/*.md"),
        ("class-anychar", "src/**/n*d[k-m]e?txt"),
        ("brace-anychar", "src/**/{tob,crazy}/?*.{png,txt}"),
    ];

    fn build_globset(pattern: &str) -> globset::GlobMatcher {
        GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .unwrap()
            .compile_matcher()
    }
    fn build_wax(pattern: &str) -> wax::Glob<'static> {
        wax::Glob::new(pattern).unwrap().into_owned()
    }

    println!(
        "{:<20}   {:>10}   {:>10}   {:>10}   {:>10}   {:>10}   {:>10}",
        "Pattern (total B/matcher)", "globstar", "ssm", "DFA", "PikeVM", "globset", "wax"
    );
    println!(
        "{:-<20}   {:->10}   {:->10}   {:->10}   {:->10}   {:->10}   {:->10}",
        "", "", "", "", "", "", ""
    );

    let trials = 9;
    for &(label, pattern) in patterns {
        let public_total = median_total(trials, || {
            let (_v, inline, heap) = measure(|| build_public(pattern));
            inline + heap
        });
        let ssm_total = median_total(trials, || {
            let (_v, inline, heap) = measure(|| build_segment(pattern));
            inline + heap
        });
        let dfa_total = median_total(trials, || {
            let (_v, inline, heap) = measure(|| build_dfa(pattern));
            inline + heap
        });
        let pike_total = median_total(trials, || {
            let (_v, inline, heap) = measure(|| build_pikevm(pattern));
            inline + heap
        });
        let gs_total = median_total(trials, || {
            let (_v, inline, heap) = measure(|| build_globset(pattern));
            inline + heap
        });
        let wax_total = median_total(trials, || {
            let (_v, inline, heap) = measure(|| build_wax(pattern));
            inline + heap
        });

        println!(
            "{:<20}   {:>10}   {:>10}   {:>10}   {:>10}   {:>10}   {:>10}",
            label, public_total, ssm_total, dfa_total, pike_total, gs_total, wax_total
        );
    }
}
