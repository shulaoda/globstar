//! Single-pattern head-to-head: `globstar` against third-party
//! matchers. Mirrors the JS-side `matcher_single.js` so the same
//! `(pattern, path)` corpus drives both rows in the comparison table.
//!
//! Each pattern reports two timings:
//!
//! - `compile` — build a fresh matcher per call and run its first
//!   match: regex-backed matchers build their search cache on first
//!   use, so the build alone undercounts them.
//! - `match`   — precompiled matcher, batch-matched against every
//!   entry in [`PATHS`] per iteration (walker model).
//!
//! `fast_glob`'s API is stateless — `glob_match(pattern, path)` does
//! the parse and the match in one call — so its `compile` and `match`
//! rows would coincide. We emit a single `fast_glob` row that does
//! the per-call parse + match work, matching what the JS bench
//! prints.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fast_glob::glob_match as fg_glob_match;
use globset::GlobBuilder;
use globstar::{CompileOptions, Glob};
use std::path::Path;
use wax::Pattern as WaxPattern;

/// Same 7 patterns the JS bench uses (`packages/bench/benches/matcher_single.js`).
const PATTERNS: &[(&str, &str)] = &[
    ("literal", "src/main.rs"),
    ("simple-wildcard", "src/*.ts"),
    ("globstar", "src/**/*.ts"),
    ("brace-suffix", "**/*.{ts,tsx,js,jsx}"),
    ("reject-prefilter", "**/*.md"),
    ("class-anychar", "src/**/n*d[k-m]e?txt"),
    ("brace-anychar", "src/**/{tob,crazy}/?*.{png,txt}"),
];

/// Same 11 paths the JS bench uses.
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

/// `dot` defaults to `true` in both runtimes, so `**/*.md` doesn't
/// filter out dotfiles — keeps Rust and JS results comparable.
const DOT: bool = true;

/// Public-API build — the "globstar (Rust)" column in BENCHMARKS.md.
fn build_public(pattern: &str) -> Glob {
    Glob::new_with(pattern, CompileOptions::default().dot(DOT)).expect("compile")
}

/// Every library must give globstar's answer on every (pattern, path)
/// pair, so the rows time the same answers. Runs once before timing.
fn assert_all_agree() {
    for &(label, pattern) in PATTERNS {
        let public = build_public(pattern);
        let gs = strict_globset(pattern);
        let wax_g = wax::Glob::new(pattern).expect("wax parse");
        for &path in PATHS {
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

fn bench_compile(c: &mut Criterion, label: &str, pattern: &str) {
    let first = PATHS[0];
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
    // fast_glob has no separate compile step; we measure it inside
    // the per-call match below where a single call is parse+match.
    group.finish();
}

fn bench_match(c: &mut Criterion, label: &str, pattern: &str) {
    let public = build_public(pattern);
    let gs = strict_globset(pattern);
    let wax_g = wax::Glob::new(pattern).expect("wax parse").into_owned();

    let mut group = c.benchmark_group(format!("match_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(public.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("globset", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(gs.is_match(black_box(Path::new(p))));
            }
        });
    });
    group.bench_function("wax", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(wax_g.is_match(black_box(*p)));
            }
        });
    });
    // Stateless fast_glob — parse + match per call, batched over PATHS
    // so its number sits on the same axis as the others.
    group.bench_function("fast_glob", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(fg_glob_match(black_box(pattern), black_box(*p)));
            }
        });
    });
    group.finish();
}

fn bench_all(c: &mut Criterion) {
    assert_all_agree();
    for &(label, pattern) in PATTERNS {
        bench_compile(c, label, pattern);
        bench_match(c, label, pattern);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
