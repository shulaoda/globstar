//! Single patterns against globset, wax and fast_glob, on the corpus the
//! JS benches read. Three rows per pattern: `build` (a fresh matcher),
//! `compile` (the build and its first call, a non-match) and `match` (a
//! compiled matcher over every path of [`PATHS`]). `fast_glob` is
//! stateless, so it has a `match` row only.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fast_glob::glob_match as fg_glob_match;
use globset::GlobBuilder;
use globstar::{CompileOptions, Glob};
use std::path::Path;
use wax::Program as WaxPattern;

mod common;
use common::{MISS, PATHS, SINGLE};

/// As in JS, so the two runtimes' rows compare.
const DOT: bool = true;

fn build_public(pattern: &str) -> Glob {
    Glob::new_with(pattern, CompileOptions::default().dot(DOT)).expect("compile")
}

/// Every library gives globstar's answer on every pair, so the rows time
/// the same answers.
fn assert_all_agree() {
    for &(label, pattern) in SINGLE.iter() {
        let public = build_public(pattern);
        let gs = strict_globset(pattern);
        let wax_g = wax::Glob::new(pattern).expect("wax parse");
        assert!(
            !public.is_match(MISS.as_bytes()),
            "`{label}` matches {MISS}"
        );
        for &path in PATHS.iter() {
            let want = public.is_match(path.as_bytes());
            for (name, got) in [
                ("globset", gs.is_match(Path::new(path))),
                ("wax", wax_g.is_match(path)),
                ("fast_glob", fg_glob_match(pattern, path)),
            ] {
                assert_eq!(
                    want, got,
                    "globstar vs {name} disagree on `{label}` `{pattern}` / `{path}`"
                );
            }
        }
    }
}

fn strict_globset(pattern: &str) -> globset::GlobMatcher {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .expect("globset parse")
        .compile_matcher()
}

fn bench_build(c: &mut Criterion, label: &str, pattern: &str) {
    let mut group = c.benchmark_group(format!("build_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| black_box(build_public(black_box(pattern))));
    });
    group.bench_function("globset", |b| {
        b.iter(|| black_box(strict_globset(black_box(pattern))));
    });
    group.bench_function("wax", |b| {
        b.iter(|| {
            black_box(
                wax::Glob::new(black_box(pattern))
                    .expect("wax parse")
                    .into_owned(),
            )
        });
    });
    group.finish();
}

/// Regex-backed matchers build their search cache on first use, so the
/// build alone undercounts them.
fn bench_compile(c: &mut Criterion, label: &str, pattern: &str) {
    let first = MISS;
    let mut group = c.benchmark_group(format!("compile_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| black_box(build_public(black_box(pattern)).is_match(first.as_bytes())));
    });
    group.bench_function("globset", |b| {
        b.iter(|| black_box(strict_globset(black_box(pattern)).is_match(Path::new(first))));
    });
    group.bench_function("wax", |b| {
        b.iter(|| {
            let g = wax::Glob::new(black_box(pattern))
                .expect("wax parse")
                .into_owned();
            black_box(g.is_match(first))
        });
    });
    group.finish();
}

fn bench_match(c: &mut Criterion, label: &str, pattern: &str) {
    let public = build_public(pattern);
    let gs = strict_globset(pattern);
    let wax_g = wax::Glob::new(pattern).expect("wax parse").into_owned();

    let mut group = c.benchmark_group(format!("match_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| {
            for p in PATHS.iter() {
                black_box(public.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("globset", |b| {
        b.iter(|| {
            for p in PATHS.iter() {
                black_box(gs.is_match(black_box(Path::new(p))));
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
    // Parse and match per call, over the same paths as the others.
    group.bench_function("fast_glob", |b| {
        b.iter(|| {
            for p in PATHS.iter() {
                black_box(fg_glob_match(black_box(pattern), black_box(*p)));
            }
        });
    });
    group.finish();
}

fn bench_all(c: &mut Criterion) {
    assert_all_agree();
    for &(label, pattern) in SINGLE.iter() {
        bench_build(c, label, pattern);
        bench_compile(c, label, pattern);
        bench_match(c, label, pattern);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
