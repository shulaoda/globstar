//! globstar's `match_dir` over every directory of [`DIRS`], for the
//! patterns of `matcher_single` and the sets of `matcher_multi`. No other
//! library has the call, so the rows have one column. They have a binary
//! of their own: built into the one that times `is_match`, these calls
//! made its shortest union rows read 2 to 6% slower, the library being
//! the same.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use globstar::{CompileOptions, Glob};

mod common;
use common::{DIRS, SINGLE};

/// As in `matcher_single`.
const DOT: bool = true;

fn bench_dir(c: &mut Criterion, label: &str, glob: &Glob) {
    let mut group = c.benchmark_group(format!("dir_{label}"));
    group.bench_function("globstar", |b| {
        b.iter(|| {
            for d in DIRS.iter() {
                black_box(glob.match_dir(black_box(d.as_bytes())));
            }
        });
    });
    group.finish();
}

fn bench_all(c: &mut Criterion) {
    for &(label, pattern) in SINGLE.iter() {
        let glob = Glob::new_with(pattern, CompileOptions::default().dot(DOT)).expect("compile");
        bench_dir(c, label, &glob);
    }
    for (label, patterns) in common::sets() {
        let glob = Glob::union(patterns.iter().copied()).unwrap();
        bench_dir(c, label, &glob);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
