//! Engine-level head-to-head: `globstar(DFA)` vs `globstar(PikeVM)` vs
//! `fast_glob`. Mirrors the JS-side `matcher_single.js` so the same
//! `(pattern, path)` corpus drives both rows in the comparison table.
//!
//! Each pattern reports two timings:
//!
//! - `compile` — build a fresh matcher per call (stateless model).
//! - `match`   — precompiled matcher, batch-matched against every
//!   entry in [`PATHS`] per iteration (walker model).
//!
//! `fast_glob`'s API is stateless — `glob_match(pattern, path)` does
//! the parse and the match in one call — so its `compile` and `match`
//! rows would coincide. We emit a single `fast_glob` row that does
//! the per-call parse + match work, matching what the JS bench
//! prints.
//!
//! The `globstar(DFA)` row force-builds a `ThompsonDfa` directly, so
//! we exercise the DFA path even on patterns that the high-level
//! `Glob` would dispatch to a different tier. PikeVM is built the
//! same way to keep the comparison apples-to-apples.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fast_glob::glob_match as fg_glob_match;
use globset::GlobBuilder;
use globstar::engine::ops::lower;
use globstar::engine::pikevm::PikeVm;
use globstar::engine::thompson_dfa::ThompsonDfa;
use globstar::parser;
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

/// Dot=true mirrors `globstar(...)`'s default in `glob.js`
/// (`DEFAULT_OPTIONS.dot = true`), so `**/*.md` doesn't filter out
/// dotfiles — keeps Rust and JS results comparable.
const DOT: bool = true;

/// Build a `ThompsonDfa` for `pattern`. Patterns that cap out the DFA
/// state limit return `Err(program)`; for our corpus none should hit
/// the cap, so we panic with the pattern name on overflow rather than
/// silently fall back to PikeVM.
fn build_dfa(pattern: &str) -> Box<ThompsonDfa> {
    let ast = parser::parse(pattern.as_bytes()).expect("parse");
    let program = lower(&ast.body, false);
    ThompsonDfa::build(program, DOT).unwrap_or_else(|_| panic!("DFA cap exceeded on `{pattern}`"))
}

fn build_pikevm(pattern: &str) -> PikeVm {
    let ast = parser::parse(pattern.as_bytes()).expect("parse");
    let program = lower(&ast.body, false);
    PikeVm::new(program, DOT)
}

/// Sanity check: DFA, PikeVM, and fast_glob must all agree on every
/// (pattern, path) pair. Runs once per group before timing starts.
fn assert_all_agree() {
    for &(label, pattern) in PATTERNS {
        let dfa = build_dfa(pattern);
        let pike = build_pikevm(pattern);
        for &path in PATHS {
            let d = dfa.is_match(path.as_bytes());
            let p = pike.is_match(path.as_bytes());
            let f = fg_glob_match(pattern, path);
            assert_eq!(
                d, p,
                "DFA vs PikeVM disagree on `{label}` `{pattern}` / `{path}`"
            );
            assert_eq!(
                d, f,
                "globstar vs fast_glob disagree on `{label}` `{pattern}` / `{path}`"
            );
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
    let mut group = c.benchmark_group(format!("compile_{label}"));
    group.bench_function("globstar_dfa", |b| {
        b.iter(|| black_box(build_dfa(black_box(pattern))));
    });
    group.bench_function("globstar_pikevm", |b| {
        b.iter(|| black_box(build_pikevm(black_box(pattern))));
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
    // fast_glob has no separate compile step; we measure it inside
    // the per-call match below where a single call is parse+match.
    group.finish();
}

fn bench_match(c: &mut Criterion, label: &str, pattern: &str) {
    let dfa = build_dfa(pattern);
    let pike = build_pikevm(pattern);
    let gs = strict_globset(pattern);
    let wax_g = wax::Glob::new(pattern).expect("wax parse").into_owned();

    let mut group = c.benchmark_group(format!("match_{label}"));
    group.bench_function("globstar_dfa", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(dfa.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("globstar_pikevm", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(pike.is_match(black_box(p.as_bytes())));
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
    // so its number can be added to the other two on the same axis.
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
