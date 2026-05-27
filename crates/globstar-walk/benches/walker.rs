//! Multi-pattern walker comparison across tree scales.
//!
//! Three axes:
//! - **scale**: small (~1K) / medium (~13K) / large (~50K) files
//! - **patterns**: multi-small (4 patterns, NFA < 64, fast-path DFA) /
//!   multi-huge (10 patterns, NFA > 64, triggers `Glob::union`'s
//!   auto-OR decomposition)
//! - **lib**: globstar-walk (default API) / globstar-walk-merged
//!   (single brace-combined Glob, forces merged DFA path) / globwalk /
//!   ignore::WalkBuilder
//!
//! `wax::walk` is single-pattern only and skipped here.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use globstar_walk::Walk;
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use std::fs;
use std::path::PathBuf;

const TREE_SCALES: &[(&str, &TreeShape)] = &[
    ("small", &TREE_SMALL),
    ("medium", &TREE_MEDIUM),
    ("large", &TREE_LARGE),
];

struct TreeShape {
    /// Directory key (also bench tree dir name).
    name: &'static str,
    /// `src/<area>/module_<n>.<ext>` per (area × module × ext) =
    /// `areas.len() * modules * exts.len()` files plus `subs *
    /// areas.len() * modules` more.
    areas: usize,
    modules: usize,
    subs: usize,
    /// Test files under `tests/{unit,integration}/test_<n>.ts`.
    tests: usize,
    /// Synthetic `node_modules/pkgN/...` packages.
    pkgs: usize,
    files_per_pkg: usize,
    /// `dist/chunk_<n>.{js,js.map}`.
    dist_chunks: usize,
}

const TREE_SMALL: TreeShape = TreeShape {
    name: "small",
    areas: 1,
    modules: 5,
    subs: 1,
    tests: 5,
    pkgs: 5,
    files_per_pkg: 5,
    dist_chunks: 10,
};

const TREE_MEDIUM: TreeShape = TreeShape {
    name: "medium",
    areas: 3,
    modules: 20,
    subs: 5,
    tests: 50,
    pkgs: 50,
    files_per_pkg: 20,
    dist_chunks: 100,
};

const TREE_LARGE: TreeShape = TreeShape {
    name: "large",
    areas: 5,
    modules: 50,
    subs: 10,
    tests: 200,
    pkgs: 200,
    files_per_pkg: 30,
    dist_chunks: 500,
};

fn build_tree(shape: &TreeShape) -> PathBuf {
    let mut root = std::env::temp_dir();
    root.push(format!("glob-bench-tree-multi-{}", shape.name));
    let sentinel = root.join(".built");
    if sentinel.exists() {
        return root;
    }
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root).expect("create bench tree root");
    let touch = |rel: &str| {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::File::create(&p).expect("create file");
    };

    let area_names: &[&str] = &["cli", "server", "client", "core", "shared"];
    for area_idx in 0..shape.areas {
        let area = area_names[area_idx % area_names.len()];
        for module in 0..shape.modules {
            for ext in &["ts", "rs", "js", "md"] {
                touch(&format!("src/{area}/module_{module}.{ext}"));
                for sub in 0..shape.subs {
                    touch(&format!("src/{area}/sub{sub}/file_{module}.{ext}"));
                }
            }
        }
    }
    for i in 0..shape.tests {
        touch(&format!("tests/unit/test_{i}.ts"));
        touch(&format!("tests/integration/test_{i}.ts"));
    }
    for pkg in 0..shape.pkgs {
        for file in 0..shape.files_per_pkg {
            touch(&format!("node_modules/pkg{pkg}/src/file{file}.ts"));
            touch(&format!("node_modules/pkg{pkg}/dist/bundle{file}.js"));
        }
    }
    for i in 0..shape.dist_chunks {
        touch(&format!("dist/chunk_{i}.js"));
        touch(&format!("dist/chunk_{i}.js.map"));
    }
    fs::File::create(&sentinel).expect("create sentinel");
    root
}

// 4-pattern set — NFA stays under 64, so `Glob::union` builds one
// merged DFA on the fast path.
const PATTERNS_SMALL: &[&str] = &["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"];

// 10-pattern set — combined NFA is 223 states, triggers
// `Glob::union`'s auto-OR decomposition into per-pattern DFAs.
const PATTERNS_HUGE: &[&str] = &[
    "src/**/*.ts",
    "src/**/*.tsx",
    "tests/**/*.test.ts",
    "**/*.json",
    "**/package.json",
    "components/**/*.vue",
    "scripts/**/*.sh",
    "docs/**/*.md",
    "src/**/*.spec.ts",
    "**/*.md",
];

/// Brace-combined single-pattern equivalent — exercises the merged-
/// DFA path even when the pattern count would otherwise trigger
/// auto-OR (Glob::new doesn't probe NFA size, only Glob::union does).
fn brace_combined(patterns: &[&str]) -> String {
    format!("{{{}}}", patterns.join(","))
}

fn bench_combo(c: &mut Criterion, scale: &TreeShape, label: &str, patterns: &[&str]) {
    let root = build_tree(scale);
    let group_name = format!("walk_{}_{}", scale.name, label);
    let mut g = c.benchmark_group(&group_name);
    g.sample_size(20);

    // globstar-walk default API — Glob::union auto-routes (DFA union
    // for small NFA, Engine::Or per-pattern for huge NFA).
    g.bench_function("globstar_walk", |b| {
        b.iter(|| {
            let w = Walk::from_patterns(black_box(patterns), &root).unwrap();
            black_box(w.filter(|r| r.is_ok()).count())
        });
    });

    // Forced merged DFA — feed the brace-combined string through
    // Walk::new (single-pattern path skips the union NFA-size probe).
    let combined = brace_combined(patterns);
    g.bench_function("globstar_walk_merged", |b| {
        b.iter(|| {
            let w = Walk::new(black_box(combined.as_str()), &root).unwrap();
            black_box(w.filter(|r| r.is_ok()).count())
        });
    });

    g.bench_function("globwalk", |b| {
        b.iter(|| {
            black_box(
                globwalk::GlobWalkerBuilder::from_patterns(&root, black_box(patterns))
                    .build()
                    .unwrap()
                    .filter_map(|e| e.ok())
                    .count(),
            )
        });
    });

    g.bench_function("ignore", |b| {
        b.iter(|| {
            let mut ob = OverrideBuilder::new(&root);
            for p in patterns {
                ob.add(black_box(p)).unwrap();
            }
            let ov = ob.build().unwrap();
            let mut wb = WalkBuilder::new(&root);
            wb.standard_filters(false);
            wb.hidden(false);
            wb.overrides(ov);
            black_box(
                wb.build()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .count(),
            )
        });
    });

    g.finish();
}

fn run(c: &mut Criterion) {
    for (_, shape) in TREE_SCALES {
        bench_combo(c, shape, "multi-small", PATTERNS_SMALL);
        bench_combo(c, shape, "multi-huge", PATTERNS_HUGE);
    }
}

criterion_group!(benches, run);
criterion_main!(benches);
