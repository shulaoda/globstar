//! Multi-pattern bench: `Glob::union` against third-party baselines,
//! mirroring JS's `matcher_multi.js`.
//!
//! Columns:
//!   globstar       — `Glob::union`
//!   globset / wax / fast_glob — third-party baselines

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fast_glob::glob_match as fg_glob_match;
use globset::GlobBuilder;
use globstar::Glob;
use std::path::Path;
use wax::Pattern as WaxPattern;

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

fn pattern_sets() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("solo-globstar", vec!["**/*.ts"]),
        (
            "brace-equiv-4",
            vec!["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"],
        ),
        (
            "brace-equiv-8",
            vec![
                "**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.mjs", "**/*.cjs", "**/*.mts",
                "**/*.cts",
            ],
        ),
        (
            "mixed-roots",
            vec!["src/**/*.ts", "tests/**/*.ts", "lib/**/*.js"],
        ),
        (
            "huge-set-pos",
            vec![
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
    ]
}

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

fn build_wax(patterns: &[&str]) -> Vec<wax::Glob<'static>> {
    patterns
        .iter()
        .map(|p| wax::Glob::new(p).expect("wax parse").into_owned())
        .collect()
}

// ---------- COMPILE bench ----------
fn bench_compile(c: &mut Criterion, label: &str, patterns: &[&str]) {
    let mut group = c.benchmark_group(format!("compile_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| black_box(build_globstar(black_box(patterns))));
    });
    group.bench_function("globset", |b| {
        b.iter(|| black_box(build_globset(black_box(patterns))));
    });
    group.bench_function("wax_or", |b| {
        b.iter(|| black_box(build_wax(black_box(patterns))));
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
            for p in PATHS {
                black_box(gs.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("globset", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(gset.is_match(black_box(Path::new(p))));
            }
        });
    });
    group.bench_function("wax_or", |b| {
        b.iter(|| {
            for p in PATHS {
                let any = wax_g.iter().any(|g| g.is_match(black_box(*p)));
                black_box(any);
            }
        });
    });
    group.bench_function("fast_glob_or", |b| {
        b.iter(|| {
            for p in PATHS {
                let any = patterns
                    .iter()
                    .any(|pat| fg_glob_match(black_box(pat), black_box(*p)));
                black_box(any);
            }
        });
    });
    group.finish();
}

fn bench_all(c: &mut Criterion) {
    for (label, patterns) in pattern_sets() {
        bench_compile(c, label, &patterns);
        bench_match(c, label, &patterns);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
