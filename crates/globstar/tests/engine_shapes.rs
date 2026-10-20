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

// Rust only: JS steps every graph NFA past 32 states sparsely. Past 64
// states the graph NFA keeps dense rows while they fit 4 KiB (170 states;
// 171 do not) and no position leads to more than 48 literals; either form
// answers as the members do one by one.
#[test]
fn a_graph_past_64_states_keeps_dense_rows_within_bounds() {
    let lits = |n: usize| -> Vec<String> { (0..n).map(|i| format!("dir{i}/file{i}")).collect() };
    let wilds =
        |n: usize| -> Vec<String> { (0..n).map(|i| format!("src/*{i}/**/?*b{i}/*.ts")).collect() };
    let paths = [
        "",
        "x",
        "dir3",
        "dir3/file3",
        "dir3/file4",
        "dir3/file3/x",
        "dir47/file47",
        "dir48/file48",
        "dir48",
        "src",
        "src/a7",
        "src/a7/x",
        "src/a7/x/yb7/z.ts",
        "src/a7/yb7/z.ts",
        "src/a7/b7/z.ts",
        "src/a7/x/y/wb7/z.ts",
        "src/a54/q/wb54/k.ts",
        "src/a55/q/wb55/k.ts",
        "src/a7/x/yb7/z.js",
    ];
    for (members, nfa) in [
        (lits(48), "wide"),
        (lits(49), "sparse"),
        (wilds(55), "wide"),
        (wilds(56), "sparse"),
    ] {
        let union = Glob::union(members.iter().map(String::as_str)).unwrap();
        let shape = union.shape();
        assert!(
            shape.ends_with(&format!("nfa {nfa}")),
            "{} x{}",
            members[0],
            members.len()
        );
        let each: Vec<Glob> = members.iter().map(|p| Glob::new(p).unwrap()).collect();
        for path in paths {
            let p = path.as_bytes();
            let any = |f: &dyn Fn(&Glob) -> bool| each.iter().any(f);
            assert_eq!(union.is_match(p), any(&|g| g.is_match(p)), "{path:?}");
            let dir = union.match_dir(p);
            assert_eq!(
                dir.matches(),
                any(&|g| g.match_dir(p).matches()),
                "{path:?}"
            );
            let below = any(&|g| g.match_dir(p).may_match_below());
            assert_eq!(dir.may_match_below(), below, "{path:?}");
        }
    }
}
