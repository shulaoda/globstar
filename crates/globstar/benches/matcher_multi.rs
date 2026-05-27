//! Multi-pattern bench with explicit union vs OR rows for both
//! engines, mirroring JS's `matcher_multi.js`.
//!
//! Columns:
//!   globstar       — `Glob::union` default API (auto-OR's at NFA > 64)
//!   DFA union      — forced merged DFA (bypass auto-OR via direct
//!                     `ThompsonDfa::build`)
//!   DFA OR         — N independent `Glob::new` matchers OR'd at match
//!   PikeVM union   — forced merged PikeVM (direct `PikeVm::new`)
//!   PikeVM OR      — N independent `PikeVm::new` matchers OR'd
//!   globset / wax / fast_glob / glob — third-party baselines

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fast_glob::glob_match as fg_glob_match;
use globset::GlobBuilder;
use globstar::Glob as GcGlob;
use globstar::ast::Node;
use globstar::engine::ops::lower;
use globstar::engine::pikevm::PikeVm;
use globstar::engine::thompson_dfa::ThompsonDfa;
use globstar::factor::factor_branches;
use globstar::parser;
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

fn has_brace(patterns: &[&str]) -> bool {
    patterns.iter().any(|p| p.contains('{'))
}

// ---------- globstar configurations ----------

/// `Glob::union` default API — auto-OR's internally at NFA > 64.
fn build_globstar(patterns: &[&str]) -> GcGlob {
    GcGlob::union(patterns.iter().copied()).unwrap()
}

/// Forced merged DFA — bypass auto-OR by feeding the merged program
/// directly to `ThompsonDfa::build`. Used to measure the wide-path
/// behavior independent of the dispatch policy.
fn build_dfa_union(patterns: &[&str]) -> Box<ThompsonDfa> {
    let bodies: Vec<Node> = patterns
        .iter()
        .map(|p| parser::parse(p.as_bytes()).expect("parse").body)
        .collect();
    let merged = factor_branches(bodies);
    let program = lower(&merged, false);
    ThompsonDfa::build(program, true).expect("DFA build")
}

/// Per-pattern DFA — N independent `Glob::new` matchers, OR'd at
/// match time.
fn build_dfa_or(patterns: &[&str]) -> Vec<GcGlob> {
    patterns.iter().map(|p| GcGlob::new(p).unwrap()).collect()
}

/// Forced merged PikeVM — direct `PikeVm::new` on the factored
/// program; no auto-OR.
fn build_pikevm_union(patterns: &[&str]) -> PikeVm {
    let bodies: Vec<Node> = patterns
        .iter()
        .map(|p| parser::parse(p.as_bytes()).expect("parse").body)
        .collect();
    let merged = factor_branches(bodies);
    let program = lower(&merged, false);
    PikeVm::new(program, true)
}

/// Per-pattern PikeVM — N independent matchers OR'd at match time.
fn build_pikevm_or(patterns: &[&str]) -> Vec<PikeVm> {
    patterns
        .iter()
        .map(|p| {
            let ast = parser::parse(p.as_bytes()).expect("parse");
            let program = lower(&ast.body, false);
            PikeVm::new(program, true)
        })
        .collect()
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

fn build_glob(patterns: &[&str]) -> Vec<glob::Pattern> {
    patterns
        .iter()
        .map(|p| glob::Pattern::new(p).expect("glob parse"))
        .collect()
}

fn glob_opts() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}

// ---------- COMPILE bench ----------
fn bench_compile(c: &mut Criterion, label: &str, patterns: &[&str]) {
    let mut group = c.benchmark_group(format!("compile_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| black_box(build_globstar(black_box(patterns))));
    });
    group.bench_function("DFA_union", |b| {
        b.iter(|| black_box(build_dfa_union(black_box(patterns))));
    });
    group.bench_function("DFA_OR", |b| {
        b.iter(|| black_box(build_dfa_or(black_box(patterns))));
    });
    group.bench_function("PikeVM_union", |b| {
        b.iter(|| black_box(build_pikevm_union(black_box(patterns))));
    });
    group.bench_function("PikeVM_OR", |b| {
        b.iter(|| black_box(build_pikevm_or(black_box(patterns))));
    });
    group.bench_function("globset", |b| {
        b.iter(|| black_box(build_globset(black_box(patterns))));
    });
    group.bench_function("wax_or", |b| {
        b.iter(|| black_box(build_wax(black_box(patterns))));
    });
    if !has_brace(patterns) {
        group.bench_function("glob_or", |b| {
            b.iter(|| black_box(build_glob(black_box(patterns))));
        });
    }
    group.finish();
}

// ---------- MATCH bench ----------
fn bench_match(c: &mut Criterion, label: &str, patterns: &[&str]) {
    let gs = build_globstar(patterns);
    let dfa_u = build_dfa_union(patterns);
    let dfa_or = build_dfa_or(patterns);
    let pike_u = build_pikevm_union(patterns);
    let pike_or = build_pikevm_or(patterns);
    let gset = build_globset(patterns);
    let wax_g = build_wax(patterns);
    let glob_g = if !has_brace(patterns) {
        Some(build_glob(patterns))
    } else {
        None
    };
    let go = glob_opts();

    let mut group = c.benchmark_group(format!("match_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(gs.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("DFA_union", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(dfa_u.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("DFA_OR", |b| {
        b.iter(|| {
            for p in PATHS {
                let any = dfa_or.iter().any(|g| g.is_match(black_box(p.as_bytes())));
                black_box(any);
            }
        });
    });
    group.bench_function("PikeVM_union", |b| {
        b.iter(|| {
            for p in PATHS {
                black_box(pike_u.is_match(black_box(p.as_bytes())));
            }
        });
    });
    group.bench_function("PikeVM_OR", |b| {
        b.iter(|| {
            for p in PATHS {
                let any = pike_or.iter().any(|g| g.is_match(black_box(p.as_bytes())));
                black_box(any);
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
    if let Some(gp) = &glob_g {
        group.bench_function("glob_or", |b| {
            b.iter(|| {
                for p in PATHS {
                    let any = gp.iter().any(|g| g.matches_with(black_box(*p), go));
                    black_box(any);
                }
            });
        });
    }
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
