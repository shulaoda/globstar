//! The engine's routing: what each pattern in `fixtures/engine-shapes.tsv`
//! compiles to. A slower path (everything sent to the NFA, no shared
//! nodes, no affix matchers) still answers right, so only this sees it.
//! Twin of the same check in packages/globstar/tests/compiler-stages.mjs.

use globstar::Glob;

const CASES: &str = include_str!("../../../fixtures/engine-shapes.tsv");

#[test]
fn patterns_compile_to_their_shapes() {
    let mut rows = 0;
    for line in CASES.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let want = fields.next().unwrap();
        let patterns: Vec<&str> = fields.collect();
        let glob = match patterns.as_slice() {
            [one] => Glob::new(one),
            many => Glob::union(many.iter().copied()),
        }
        .unwrap();
        assert_eq!(glob.shape(), want, "{:.60}", patterns.join(" + "));
        rows += 1;
    }
    assert!(rows >= 20, "only {rows} rows");
}
