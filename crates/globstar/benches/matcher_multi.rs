//! Multi-pattern bench: `Glob::union` against third-party baselines,
//! mirroring JS's `matcher_multi.js`.
//!
//! Each library matches a set the way it offers to: globset through one
//! `GlobSet`, wax through `wax::any` (one program over the set). The
//! fast-glob crate has no such facility, so its column loops its
//! single-pattern function over the set, stopping at the first hit, as a
//! caller would.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fast_glob::glob_match as fg_glob_match;
use globset::GlobBuilder;
use globstar::Glob;
use std::path::Path;
use wax::Program as WaxPattern;

mod common;
use common::{MISS, PATHS};

// ---------- globstar configurations ----------

fn build_globstar(patterns: &[&str]) -> Glob {
    Glob::union(patterns.iter().copied()).unwrap()
}

// ---------- third-party libs ----------

fn build_globset(patterns: &[&str]) -> globset::GlobSet {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        b.add(
            GlobBuilder::new(p)
                .literal_separator(true)
                .build()
                .expect("globset parse"),
        );
    }
    b.build().expect("globset build")
}

fn build_wax<'a>(patterns: &[&'a str]) -> wax::Any<'a> {
    wax::any(patterns.iter().copied()).expect("wax any")
}

// ---------- BUILD bench ----------
/// The build alone: what a library does before its first match.
fn bench_build(c: &mut Criterion, label: &str, patterns: &[&str]) {
    let mut group = c.benchmark_group(format!("build_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| black_box(build_globstar(black_box(patterns))));
    });
    group.bench_function("globset", |b| {
        b.iter(|| black_box(build_globset(black_box(patterns))));
    });
    group.bench_function("wax", |b| {
        b.iter(|| black_box(build_wax(black_box(patterns))));
    });
    group.finish();
}

// ---------- COMPILE bench ----------
/// A build and its first match: regex-backed matchers build their
/// search cache on first use, so the build alone undercounts them. The
/// match misses, so separate matchers all run.
fn bench_compile(c: &mut Criterion, label: &str, patterns: &[&str]) {
    let first = MISS;
    let mut group = c.benchmark_group(format!("compile_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| black_box(build_globstar(black_box(patterns)).is_match(first.as_bytes())));
    });
    group.bench_function("globset", |b| {
        b.iter(|| black_box(build_globset(black_box(patterns)).is_match(Path::new(first))));
    });
    group.bench_function("wax", |b| {
        b.iter(|| black_box(build_wax(black_box(patterns)).is_match(first)));
    });
    group.finish();
}

// ---------- MATCH bench ----------
fn bench_match(c: &mut Criterion, label: &str, patterns: &[&str]) {
    let gs = build_globstar(patterns);
    let gset = build_globset(patterns);
    let wax_g = build_wax(patterns);

    let mut group = c.benchmark_group(format!("match_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| {
            for p in PATHS.iter() {
                black_box(gs.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("globset", |b| {
        b.iter(|| {
            for p in PATHS.iter() {
                black_box(gset.is_match(black_box(Path::new(p))));
            }
        });
    });
    group.bench_function("wax", |b| {
        b.iter(|| {
            for p in PATHS.iter() {
                black_box(wax_g.is_match(black_box(*p)));
            }
        });
    });
    group.bench_function("fast_glob", |b| {
        b.iter(|| {
            for p in PATHS.iter() {
                let any = patterns
                    .iter()
                    .any(|pat| fg_glob_match(black_box(pat), black_box(*p)));
                black_box(any);
            }
        });
    });
    group.finish();
}

/// Every library must give globstar's answer on every (set, path) pair,
/// so the rows time the same answers. Runs once before timing.
fn assert_all_agree() {
    for (label, patterns) in common::sets() {
        let union = build_globstar(&patterns);
        let gset = build_globset(&patterns);
        let wax_g = build_wax(&patterns);
        assert!(!union.is_match(MISS.as_bytes()), "`{label}` matches {MISS}");
        for &path in PATHS.iter() {
            let want = union.is_match(path.as_bytes());
            for (name, got) in [
                ("globset", gset.is_match(Path::new(path))),
                ("wax", wax_g.is_match(path)),
                (
                    "fast_glob",
                    patterns.iter().any(|pat| fg_glob_match(pat, path)),
                ),
            ] {
                assert_eq!(
                    want, got,
                    "globstar vs {name} disagree on `{label}` / `{path}`"
                );
            }
        }
    }
}

fn bench_all(c: &mut Criterion) {
    assert_all_agree();
    for (label, patterns) in common::sets() {
        bench_build(c, label, &patterns);
        bench_compile(c, label, &patterns);
        bench_match(c, label, &patterns);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
