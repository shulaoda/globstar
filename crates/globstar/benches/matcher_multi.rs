//! Multi-pattern bench: `Glob::union` against third-party baselines,
//! mirroring JS's `matcher_multi.js`.
//!
//! Columns:
//!   globstar       — `Glob::union`
//!   globset / wax / fast_glob — third-party baselines

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use fast_glob::glob_match as fg_glob_match;
use globset::GlobBuilder;
use globstar::Glob;
use std::path::Path;
use wax::Pattern as WaxPattern;

/// A path no row matches. A first match on it runs every part a matcher
/// left for its first use (in a list of separate matchers, each one).
const MISS: &str = "target/debug/build/foo";

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

/// The `ignore-list-40` set, shared with the JS bench and the memory tool.
const IGNORE_LIST: &str = include_str!("../../../fixtures/bench-ignore-list.txt");

/// The `file-list-100` set: 100 literal files over 10 folders, as a
/// generated manifest lists them. Two segments vary, so the union does
/// not factor into one chain and compiles to the sparse graph NFA.
fn file_list() -> Vec<&'static str> {
    const AREAS: [&str; 10] = [
        "cli", "server", "client", "core", "shared", "utils", "hooks", "store", "api", "ui",
    ];
    (0..100)
        .map(|i| &*Box::leak(format!("src/{}/mod_{i}.ts", AREAS[i % AREAS.len()]).into_boxed_str()))
        .collect()
}

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
        (
            "ignore-list-40",
            IGNORE_LIST
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect(),
        ),
        ("file-list-100", file_list()),
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
    group.bench_function("wax_or", |b| {
        b.iter(|| {
            let any = build_wax(black_box(patterns))
                .iter()
                .any(|g| g.is_match(first));
            black_box(any)
        });
    });
    group.finish();
}

/// The first match alone, on matchers just built (the build is not
/// timed): what a library leaves for its first use.
fn bench_first(c: &mut Criterion, label: &str, patterns: &[&str]) {
    let mut group = c.benchmark_group(format!("first_{label}"));
    group.bench_function("globstar", |b| {
        b.iter_batched_ref(
            || build_globstar(patterns),
            |g| black_box(g.is_match(MISS.as_bytes())),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("globset", |b| {
        b.iter_batched_ref(
            || build_globset(patterns),
            |g| black_box(g.is_match(Path::new(MISS))),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("wax_or", |b| {
        b.iter_batched_ref(
            || build_wax(patterns),
            |gs| black_box(gs.iter().any(|g| g.is_match(MISS))),
            BatchSize::SmallInput,
        );
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

/// Every library must give globstar's answer on every (set, path) pair,
/// so the rows time the same answers. Runs once before timing.
fn assert_all_agree() {
    for (label, patterns) in pattern_sets() {
        let union = build_globstar(&patterns);
        let gset = build_globset(&patterns);
        let wax_g = build_wax(&patterns);
        assert!(!union.is_match(MISS.as_bytes()), "`{label}` matches {MISS}");
        for &path in PATHS {
            let want = union.is_match(path.as_bytes());
            for (name, got) in [
                ("globset", gset.is_match(Path::new(path))),
                ("wax", wax_g.iter().any(|g| g.is_match(path))),
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
    for (label, patterns) in pattern_sets() {
        bench_compile(c, label, &patterns);
        bench_first(c, label, &patterns);
        bench_match(c, label, &patterns);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
