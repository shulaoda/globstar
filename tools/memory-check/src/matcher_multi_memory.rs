//! Multi-pattern memory comparison: globstar union (one merged matcher),
//! globset (one GlobSet over Aho-Corasick + N regexes), wax (one
//! `wax::any` program over the set). Mirrors the JS-side
//! `matcher_multi.js`; a cell is what the matcher keeps once it has
//! matched every bench path, or with `--peak` the most its build and
//! those matches held at once (`counter::kept_and_peak`).

use std::path::Path;

use globset::GlobBuilder;
use globstar::Glob;
use wax::Program as _;

#[path = "../../../crates/globstar/benches/common/mod.rs"]
mod common;
mod counter;
use common::PATHS;
use counter::kept_and_peak;

const TRIALS: usize = 9;

fn main() {
    let peak = std::env::args().any(|a| a == "--peak");
    let cell = |(kept, most): (usize, usize)| if peak { most } else { kept };
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
        let globstar = cell(kept_and_peak(
            TRIALS,
            &PATHS,
            || Glob::union(patterns.iter().copied()).unwrap(),
            |g, p| g.is_match(p.as_bytes()),
        ));
        let globset = cell(kept_and_peak(
            TRIALS,
            &PATHS,
            || build_globset(&patterns),
            |g, p| g.is_match(Path::new(p)),
        ));
        let wax = cell(kept_and_peak(
            TRIALS,
            &PATHS,
            || wax::any(patterns.iter().copied()).unwrap(),
            |g, p| g.is_match(p),
        ));
        let n = patterns.len();
        println!("{label:<18} {n:>3} {globstar:>12} {globset:>12} {wax:>12}");
    }
}
