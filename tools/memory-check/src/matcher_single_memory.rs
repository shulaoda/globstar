//! Cross-runtime memory comparison: measures the production matcher on
//! the single-pattern bench rows (`fixtures/bench-single.tsv`, which the
//! JS bench reads too), so the rows line up with the JS column. A cell
//! is what one matcher keeps once it has matched every bench path
//! (`counter::kept`).

use std::hint::black_box;
use std::path::Path;

use globset::GlobBuilder;
use globstar::{CompileOptions, Glob};
use wax::Program as _;

#[path = "../../../crates/globstar/benches/common/mod.rs"]
mod common;
mod counter;
use common::{PATHS, SINGLE};
use counter::kept;

const TRIALS: usize = 9;

fn main() {
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

    println!(
        "{:<20}   {:>10}   {:>10}   {:>10}",
        "Pattern (total B/matcher)", "globstar", "globset", "wax"
    );
    println!("{:-<20}   {:->10}   {:->10}   {:->10}", "", "", "", "");

    for &(label, pattern) in SINGLE.iter() {
        let globstar = kept(
            TRIALS,
            || build_public(pattern),
            |g| {
                for p in PATHS.iter() {
                    black_box(g.is_match(p.as_bytes()));
                }
            },
        );
        let globset = kept(
            TRIALS,
            || build_globset(pattern),
            |g| {
                for p in PATHS.iter() {
                    black_box(g.is_match(Path::new(p)));
                }
            },
        );
        let wax = kept(
            TRIALS,
            || build_wax(pattern),
            |g| {
                for p in PATHS.iter() {
                    black_box(g.is_match(*p));
                }
            },
        );
        println!("{label:<20}   {globstar:>10}   {globset:>10}   {wax:>10}");
    }
}
