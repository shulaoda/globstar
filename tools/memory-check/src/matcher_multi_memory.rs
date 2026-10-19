//! Multi-pattern memory comparison: globstar union (one merged matcher),
//! globstar per-pattern (N independent matchers), globset (one
//! GlobSet over Aho-Corasick + N regexes), wax (N independent
//! matchers). Mirrors the JS-side `matcher_multi.js`.
//!
//! Same counting-allocator trick as `matcher_single_memory.rs`: we measure
//! the net byte-count delta around each builder and its matches on the
//! bench paths. For per-pattern
//! modes we sum the allocations of all N matchers — that's the
//! actual memory cost a caller pays.

use std::hint::black_box;
use std::path::Path;

use globset::GlobBuilder;
use globstar::Glob;
use wax::Pattern as _;

#[path = "../../../crates/globstar/benches/common/mod.rs"]
mod common;
mod counter;
use common::PATHS;
use counter::{measure_heap, median_of};

fn main() {
    fn build_globset(patterns: &[&str]) -> globset::GlobSet {
        let mut b = globset::GlobSetBuilder::new();
        for p in patterns {
            b.add(GlobBuilder::new(p).literal_separator(true).build().unwrap());
        }
        b.build().unwrap()
    }

    let trials = 9;

    println!(
        "{:<18} {:>3} {:>12} {:>12} {:>12} {:>12}",
        "Pattern set", "N", "gs_union", "gs_per", "globset", "wax_per"
    );
    println!(
        "{:-<18} {:->3} {:->12} {:->12} {:->12} {:->12}",
        "", "", "", "", "", ""
    );

    for (label, patterns) in common::sets() {
        let n = patterns.len();

        // globstar union — one merged Glob.
        let gs_union = median_of(trials, || {
            let (_v, heap) = measure_heap(|| {
                let g = Glob::union(patterns.iter().copied()).unwrap();
                for p in PATHS.iter() {
                    black_box(g.is_match(p.as_bytes()));
                }
                g
            });
            heap
        });

        // globstar per-pattern — N independent Glob matchers (heap of
        // a Vec<Glob> with N entries).
        let gs_per = median_of(trials, || {
            let (_v, heap) = measure_heap(|| {
                let gs = patterns
                    .iter()
                    .map(|p| Glob::new(p).unwrap())
                    .collect::<Vec<_>>();
                for p in PATHS.iter() {
                    black_box(gs.iter().any(|g| g.is_match(p.as_bytes())));
                }
                gs
            });
            heap
        });

        // globset — N regexes + Aho-Corasick prefilter, all merged.
        let globset_total = median_of(trials, || {
            let (_v, heap) = measure_heap(|| {
                let g = build_globset(&patterns);
                for p in PATHS.iter() {
                    black_box(g.is_match(Path::new(p)));
                }
                g
            });
            heap
        });

        // wax per-pattern — N independent matchers.
        let wax_per = median_of(trials, || {
            let (_v, heap) = measure_heap(|| {
                let gs = patterns
                    .iter()
                    .map(|p| wax::Glob::new(p).unwrap().into_owned())
                    .collect::<Vec<_>>();
                for p in PATHS.iter() {
                    black_box(gs.iter().any(|g| g.is_match(*p)));
                }
                gs
            });
            heap
        });

        println!(
            "{:<18} {:>3} {:>12} {:>12} {:>12} {:>12}",
            label, n, gs_union, gs_per, globset_total, wax_per
        );
    }
}
