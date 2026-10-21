//! Multi-pattern memory comparison: globstar union (one merged matcher),
//! globset (one GlobSet over Aho-Corasick + N regexes), wax (one
//! `wax::any` program over the set). Mirrors the JS-side
//! `matcher_multi.js`; a cell is what the matcher keeps once it has
//! matched every bench path (`counter::kept`).

use std::path::Path;

use globset::GlobBuilder;
use globstar::Glob;
use wax::Program as _;

#[path = "../../../crates/globstar/benches/common/mod.rs"]
mod common;
mod counter;
use common::PATHS;
use counter::kept;

const TRIALS: usize = 9;

fn main() {
    let build_globset = |patterns: &[&str]| {
        let mut b = globset::GlobSetBuilder::new();
        for p in patterns {
            b.add(GlobBuilder::new(p).literal_separator(true).build().unwrap());
        }
        b.build().unwrap()
    };

    println!(
        "{:<18} {:>3} {:>12} {:>12} {:>12}",
        "Pattern set", "N", "globstar", "globset", "wax"
    );
    println!("{:-<18} {:->3} {:->12} {:->12} {:->12}", "", "", "", "", "");

    for (label, patterns) in common::sets() {
        let globstar = kept(
            TRIALS,
            &PATHS,
            || Glob::union(patterns.iter().copied()).unwrap(),
            |g, p| g.is_match(p.as_bytes()),
        );
        let globset = kept(
            TRIALS,
            &PATHS,
            || build_globset(&patterns),
            |g, p| g.is_match(Path::new(p)),
        );
        let wax = kept(
            TRIALS,
            &PATHS,
            || wax::any(patterns.iter().copied()).unwrap(),
            |g, p| g.is_match(p),
        );
        let n = patterns.len();
        println!("{label:<18} {n:>3} {globstar:>12} {globset:>12} {wax:>12}");
    }
}
