//! The bench corpus, read from `fixtures/`. The JS benches read the same
//! files (`packages/bench/benches/shared.js`), so the rows of the two
//! runtimes line up by construction. `tools/memory-check` includes this
//! module by path.
#![allow(dead_code)]

use std::sync::LazyLock;

/// A path no row matches. A first match on it runs every part a matcher
/// left for its first use (in a list of separate matchers, each one).
pub const MISS: &str = "target/debug/build/foo";

/// The 11 paths the match benches answer.
pub static PATHS: LazyLock<Vec<&str>> =
    LazyLock::new(|| lines(include_str!("../../../../fixtures/bench-paths.txt")));

/// The single-pattern rows: `(label, pattern)`.
pub static SINGLE: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| {
    lines(include_str!("../../../../fixtures/bench-single.tsv"))
        .into_iter()
        .map(|l| l.split_once('\t').expect("label<TAB>pattern"))
        .collect()
});

/// The union rows: `(label, patterns)`. `ignore-list-40` and
/// `file-list-100` are files of their own.
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
    out
}

/// The data lines of a fixture: not blank, not a `#` comment.
fn lines(text: &'static str) -> Vec<&'static str> {
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}
