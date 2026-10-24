//! What a single-pattern matcher keeps once it has matched every bench
//! path, against globset and wax; with `--peak`, the most its build and
//! those matches held at once.

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
    // `dot=true`, as in JS.
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
