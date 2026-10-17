use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use globstar::parser::parse;

const CASES: &[(&str, &str)] = &[
    ("literal", "src/main.rs"),
    ("wildcard", "src/*.ts"),
    ("globstar", "src/**/*.ts"),
    ("brace", "**/*.{ts,tsx,js,jsx}"),
    ("nested-brace", "{{a,{b,{c,d}}},e}/**/*.rs"),
    ("separator-distribution", "{**,src/**}/mod.{rs,toml}"),
    ("star-run", "src/********************************.rs"),
];

fn bench_stages(c: &mut Criterion) {
    let mut parse_group = c.benchmark_group("stage_parse");
    for &(name, pattern) in CASES {
        parse_group.bench_with_input(BenchmarkId::from_parameter(name), pattern, |b, p| {
            b.iter(|| parse(black_box(p.as_bytes())).expect("parse"));
        });
    }
    parse_group.finish();
}

criterion_group!(benches, bench_stages);
criterion_main!(benches);
