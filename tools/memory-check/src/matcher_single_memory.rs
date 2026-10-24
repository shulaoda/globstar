//! Cross-runtime memory comparison: measures the production matcher on
//! the single-pattern bench rows (`fixtures/bench-single.tsv`, which the
//! JS bench reads too), so the rows line up with the JS column. A cell
//! is what one matcher keeps once it has matched every bench path, or
//! with `--peak` the most its build and those matches held at once
//! (`counter::kept_and_peak`).

use std::path::Path;

use globset::GlobBuilder;
use globstar::{CompileOptions, Glob};
use wax::Program as _;

#[path = "../../../crates/globstar/benches/common/mod.rs"]
mod common;
mod counter;
use common::{PATHS, SINGLE};
use counter::kept_and_peak;

const TRIALS: usize = 9;

fn main() {
    let peak = std::env::args().any(|a| a == "--peak");
    let cell = |(kept, most): (usize, usize)| if peak { most } else { kept };
    // Public-API build of `globstar`. `dot=true` mirrors the JS default.
    let build_public = |pattern: &str| {
        Glob::new_with(pattern, CompileOptions::default().dot(true)).expect("compile")
    };
    let build_globset = |pattern: &str| {
        GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .unwrap()
            .compile_matcher()
    };
    let build_wax = |pattern: &str| wax::Glob::new(pattern).unwrap().into_owned();

    let unit = if peak {
        "peak B/build"
    } else {
        "total B/matcher"
    };
    println!(
        "{:<20}   {:>10}   {:>10}   {:>10}",
        format!("Pattern ({unit})"),
        "globstar",
        "globset",
        "wax"
    );
    println!("{:-<20}   {:->10}   {:->10}   {:->10}", "", "", "", "");

    for &(label, pattern) in SINGLE.iter() {
        let globstar = cell(kept_and_peak(
            TRIALS,
            &PATHS,
            || build_public(pattern),
            |g, p| g.is_match(p.as_bytes()),
        ));
        let globset = cell(kept_and_peak(
            TRIALS,
            &PATHS,
            || build_globset(pattern),
            |g, p| g.is_match(Path::new(p)),
        ));
        let wax = cell(kept_and_peak(
            TRIALS,
            &PATHS,
            || build_wax(pattern),
            |g, p| g.is_match(p),
        ));
        println!("{label:<20}   {globstar:>10}   {globset:>10}   {wax:>10}");
    }
}
