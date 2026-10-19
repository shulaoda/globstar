//! Cross-runtime memory comparison: measures the production matcher on
//! the single-pattern bench rows (`fixtures/bench-single.tsv`, which the
//! JS bench reads too), so the rows line up with the JS column.
//!
//! Counting `GlobalAlloc` wrapper tracks net bytes-allocated, and we
//! measure (after − before) around each engine's `build` and its
//! matches on the bench paths. Inline + heap = total per matcher.

use std::hint::black_box;
use std::mem::size_of_val;
use std::path::Path;

use globset::GlobBuilder;
use globstar::{CompileOptions, Glob};
use wax::Pattern as _;

#[path = "../../../crates/globstar/benches/common/mod.rs"]
mod common;
mod counter;
use common::{PATHS, SINGLE};
use counter::{measure_heap, median_of};

/// Public-API build of `globstar`. `dot=true` mirrors the
/// JS default.
fn build_public(pattern: &str) -> Glob {
    Glob::new_with(pattern, CompileOptions::default().dot(true)).expect("compile")
}

fn main() {
    fn build_globset(pattern: &str) -> globset::GlobMatcher {
        GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .unwrap()
            .compile_matcher()
    }
    fn build_wax(pattern: &str) -> wax::Glob<'static> {
        wax::Glob::new(pattern).unwrap().into_owned()
    }

    println!(
        "{:<20}   {:>10}   {:>10}   {:>10}",
        "Pattern (total B/matcher)", "globstar", "globset", "wax"
    );
    println!("{:-<20}   {:->10}   {:->10}   {:->10}", "", "", "", "");

    let trials = 9;
    for &(label, pattern) in SINGLE.iter() {
        let public_total = median_of(trials, || {
            let (v, heap) = measure_heap(|| {
                let g = build_public(pattern);
                for p in PATHS.iter() {
                    black_box(g.is_match(p.as_bytes()));
                }
                g
            });
            size_of_val(&v) + heap
        });
        let gs_total = median_of(trials, || {
            let (v, heap) = measure_heap(|| {
                let g = build_globset(pattern);
                for p in PATHS.iter() {
                    black_box(g.is_match(Path::new(p)));
                }
                g
            });
            size_of_val(&v) + heap
        });
        let wax_total = median_of(trials, || {
            let (v, heap) = measure_heap(|| {
                let g = build_wax(pattern);
                for p in PATHS.iter() {
                    black_box(g.is_match(*p));
                }
                g
            });
            size_of_val(&v) + heap
        });

        println!(
            "{:<20}   {:>10}   {:>10}   {:>10}",
            label, public_total, gs_total, wax_total
        );
    }
}
