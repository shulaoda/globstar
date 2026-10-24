//! The bench corpus, read from `fixtures/`; the JS benches read the same
//! files, so the rows of the two runtimes line up. `tools/memory-check`
//! includes this module by path.
#![allow(dead_code)]

use std::sync::LazyLock;

/// A path no row matches: a first call on it runs every part a matcher
/// left for its first use.
pub const MISS: &str = "crates/core/src/foo";

/// The 11 paths the match benches answer.
pub static PATHS: LazyLock<Vec<&str>> =
    LazyLock::new(|| lines(include_str!("../../../../fixtures/bench-paths.txt")));

/// `(label, pattern)`.
pub static SINGLE: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| {
    lines(include_str!("../../../../fixtures/bench-single.tsv"))
        .into_iter()
        .map(|l| l.split_once('\t').expect("label<TAB>pattern"))
        .collect()
});

/// `(label, patterns)`: the sets file, the three list files, the three
/// real `.gitignore` sets, and `all-cases`, every pattern of the bench but
/// those three in one union.
pub fn sets() -> Vec<(&'static str, Vec<&'static str>)> {
    let mut out: Vec<_> = lines(include_str!("../../../../fixtures/bench-sets.tsv"))
        .into_iter()
        .map(|l| {
            let mut cols = l.split('\t');
            (cols.next().unwrap(), cols.collect())
        })
        .collect();
    out.push((
        "ignore-list-40",
        lines(include_str!("../../../../fixtures/bench-ignore-list.txt")),
    ));
    out.push((
        "file-list-100",
        lines(include_str!("../../../../fixtures/bench-file-list.txt")),
    ));
    out.push((
        "gitignore-89",
        lines(include_str!(
            "../../../../fixtures/bench-gitignore-list.txt"
        )),
    ));
    let mut all: Vec<&str> = Vec::new();
    for p in SINGLE
        .iter()
        .map(|&(_, p)| p)
        .chain(out.iter().flat_map(|(_, ps)| ps.iter().copied()))
    {
        if !all.contains(&p) {
            all.push(p);
        }
    }
    out.push((
        "global-586",
        lines(include_str!(
            "../../../../fixtures/bench-gitignore-global.txt"
        )),
    ));
    out.push((
        "openssl-500",
        lines(include_str!(
            "../../../../fixtures/bench-gitignore-openssl.txt"
        )),
    ));
    out.push((
        "joomla-705",
        lines(include_str!(
            "../../../../fixtures/bench-gitignore-joomla.txt"
        )),
    ));
    out.push(("all-cases", all));
    out
}

fn lines(text: &'static str) -> Vec<&'static str> {
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}
