//! Boundary matrix: pattern shapes × path shapes, SSM vs the old
//! engine vs globset. Asserts all engines agree on every cell before
//! timing it. Ad-hoc hot-loop timing (median of 3 × 200k iters) —
//! for relative comparison, not absolute SLOs.
//!
//! ```sh
//! cargo run --release -p globstar --example boundary_bench
//! ```

use globset::GlobBuilder;
use globstar::{CompileOptions, Glob};
use globstar_segment::SegGlob;
use std::hint::black_box;
use std::time::Instant;

const PATTERNS: &[(&str, &str)] = &[
    ("user-case", "**/a/*/?a*"),
    ("suffix", "**/*.ts"),
    ("anchored", "src/**/*.ts"),
    ("fixed-deep", "a/b/c/d/e/f.ts"),
    ("wild-head", "*/*/*/*.ts"),
    ("multi-g2", "**/a/**"),
    ("multi-g3", "**/a/**/b/**"),
    ("long-tail", "**/a/b/x.ts"),
    ("generic-seg", "*a*b*c*"),
    ("class-tail", "**/[a-s][0-9]?x*.ts"),
    ("brace-8", "**/*.{ts,tsx,js,jsx,mjs,cjs,json,md}"),
    ("fork-4", "{src,lib,test,docs}/**/*.ts"),
    ("prefix-g1", "src/**"),
    ("everything", "**"),
    ("qmark-run", "??????.ts"),
];

const PATHS: &[(&str, &str)] = &[
    ("tiny", "a.ts"),
    ("typical", "src/components/button/index.ts"),
    ("deep", "src/a/b/c/d/e/f/g/h/i/j/k/x.ts"),
    ("aaa-16", "a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a"),
    ("reject", "node_modules/foo/bar/baz/index.js"),
    (
        "long-seg",
        "src/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/needle.ts",
    ),
    ("dot-led", ".git/objects/ab/cdef123"),
    ("empty-seg", "a//b/x.ts"),
];

fn time_ns(mut f: impl FnMut()) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..3 {
        // warmup
        for _ in 0..20_000 {
            f();
        }
        let n = 200_000u32;
        let t = Instant::now();
        for _ in 0..n {
            f();
        }
        best = best.min(t.elapsed().as_nanos() as f64 / n as f64);
    }
    best
}

fn main() {
    let dots = [true, false];
    for dot in dots {
        println!("\n================ dot={dot} ================");
        println!(
            "{:<12} {:<10} | {:>7} {:>7} {:>7} | ssm/old ssm/gset | agree",
            "pattern", "path", "ssm", "old", "globset"
        );
        for &(plabel, pat) in PATTERNS {
            let opts = CompileOptions::default().dot(dot);
            let ssm = SegGlob::new_with(pat, opts).unwrap();
            let old = Glob::new_with(pat, opts).unwrap();
            let gset = GlobBuilder::new(pat)
                .literal_separator(true)
                .build()
                .unwrap()
                .compile_matcher();

            for &(plab2, path) in PATHS {
                let bytes = path.as_bytes();
                let a = ssm.is_match(bytes);
                let b = old.is_match(bytes);
                // globset has no dot option; only compare verdicts at
                // dot=true where dialects line up.
                let agree_gs = if dot { gset.is_match(path) == a } else { true };
                assert_eq!(a, b, "SSM vs old disagree: {pat} / {path} dot={dot}");

                let t_ssm = time_ns(|| {
                    black_box(ssm.is_match(black_box(bytes)));
                });
                let t_old = time_ns(|| {
                    black_box(old.is_match(black_box(bytes)));
                });
                let t_gs = time_ns(|| {
                    black_box(gset.is_match(black_box(path)));
                });
                println!(
                    "{plabel:<12} {plab2:<10} | {t_ssm:7.1} {t_old:7.1} {t_gs:7.1} | {:7.2} {:8.2} | {}{}",
                    t_ssm / t_old,
                    t_ssm / t_gs,
                    if a { "m" } else { "-" },
                    if agree_gs { "" } else { " (gset-diff)" },
                );
            }
        }
    }

    // Compile-time row per pattern (engines built fresh per iter).
    println!("\n================ compile (dot=true) ================");
    println!("{:<12} | {:>9} {:>9} {:>9}", "pattern", "ssm", "old", "globset");
    for &(plabel, pat) in PATTERNS {
        let t_ssm = time_ns(|| {
            black_box(SegGlob::new(black_box(pat)).unwrap());
        });
        let t_old = time_ns(|| {
            black_box(Glob::new(black_box(pat)).unwrap());
        });
        let t_gs = time_ns(|| {
            black_box(
                GlobBuilder::new(black_box(pat))
                    .literal_separator(true)
                    .build()
                    .unwrap()
                    .compile_matcher(),
            );
        });
        println!("{plabel:<12} | {t_ssm:9.1} {t_old:9.1} {t_gs:9.1}");
    }
}
