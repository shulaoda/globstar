//! Bounded-exhaustive `match_dir` verification.
//!
//! Enumerates EVERY pattern buildable from a fixed segment alphabet (the
//! full 14-token set at 1-2 segments, a core 8-token set at 3) against
//! every path in a fixed universe, on both the default engine stack and a
//! forced PikeVm, and checks the properties that define `match_dir`
//! relative to `is_match`:
//!
//! 1. the match flag is exactly `is_match(dir)`;
//! 2. if any universe path strictly below the dir matches, the result must
//!    descend — a pruning walker must never lose a match;
//! 3. `Pruned` implies no universe descendant matches;
//! 4. the two engines agree on every `is_match` and every `match_dir`.
//!
//! Deterministic: no randomness — the whole space within the bounds is
//! covered on every run. The JS twin
//! (packages/globstar/tests/dir-exhaustive.mjs) runs the identical
//! enumeration.

use globstar::engine::ops::lower;
use globstar::engine::pikevm::PikeVm;
use globstar::parser::parse;
use globstar::{CompileOptions, DirMatch, Glob};

/// Full alphabet: literals, wildcards, classes, dot heads, braces, globstar.
const SEGMENTS: &[&str] = &[
    "a", "b", "ab", "*", "?", "a*", "*a", "[ab]", "[!a]", ".a", "{a,b}", "{a,*}", "**", ".*",
];
/// Core alphabet for the depth-3 sweep (keeps the space tractable).
const CORE: &[&str] = &["a", "b", "*", "?", "[ab]", ".a", "{a,b}", "**"];
/// Path universe segments; `c` matches nothing literal in the alphabet.
const PSEG: &[&str] = &["a", "b", "ab", "c", ".a"];

/// All `/`-joined sequences of `segs` with exactly `depth` components.
fn enumerate(segs: &[&str], depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut idx = vec![0usize; depth];
    loop {
        out.push(
            idx.iter()
                .map(|&i| segs[i])
                .collect::<Vec<_>>()
                .join("/"),
        );
        let mut d = depth;
        loop {
            if d == 0 {
                return out;
            }
            d -= 1;
            idx[d] += 1;
            if idx[d] < segs.len() {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn patterns() -> Vec<String> {
    let mut out = Vec::new();
    out.extend(enumerate(SEGMENTS, 1));
    out.extend(enumerate(SEGMENTS, 2));
    out.extend(enumerate(CORE, 3));
    out
}

fn universe() -> Vec<String> {
    let mut out = Vec::new();
    for d in 1..=3 {
        out.extend(enumerate(PSEG, d));
    }
    out
}

/// `child` is strictly below `dir` (dir + `/` + more).
fn is_below(child: &str, dir: &str) -> bool {
    child.len() > dir.len()
        && child.as_bytes()[dir.len()] == b'/'
        && child.starts_with(dir)
}

fn build_pikevm(pattern: &str, dot: bool, ci: bool) -> PikeVm {
    let ast = parse(pattern.as_bytes()).expect("parse");
    PikeVm::new(lower(&ast.body, ci), dot)
}

fn check(patterns: &[String], paths: &[String], dot: bool, ci: bool) {
    // Precompute the strictly-below relation once.
    let below: Vec<Vec<usize>> = paths
        .iter()
        .map(|d| {
            paths
                .iter()
                .enumerate()
                .filter(|(_, p)| is_below(p, d))
                .map(|(j, _)| j)
                .collect()
        })
        .collect();

    for pattern in patterns {
        let opts = CompileOptions::default().dot(dot).case_insensitive(ci);
        let default = Glob::new_with(pattern, opts).expect("compile");
        let pike = build_pikevm(pattern, dot, ci);

        let matched: Vec<bool> = paths
            .iter()
            .map(|p| default.is_match(p.as_bytes()))
            .collect();

        for (i, dir) in paths.iter().enumerate() {
            let pm = pike.is_match(dir.as_bytes());
            assert_eq!(
                matched[i], pm,
                "is_match disagreement: pattern={pattern:?} path={dir:?} dot={dot} ci={ci} \
                 default={} pikevm={pm}",
                matched[i]
            );

            let dm = default.match_dir(dir.as_bytes());
            let pdm = pike.match_dir(dir.as_bytes());
            assert_eq!(
                dm, pdm,
                "match_dir disagreement: pattern={pattern:?} dir={dir:?} dot={dot} ci={ci}"
            );

            assert_eq!(
                dm.is_match(),
                matched[i],
                "match flag != is_match: pattern={pattern:?} dir={dir:?} dot={dot} ci={ci} dm={dm:?}"
            );

            let any_below = below[i].iter().any(|&j| matched[j]);
            if any_below {
                assert!(
                    dm.should_descend(),
                    "walker would lose a match: pattern={pattern:?} dir={dir:?} dot={dot} \
                     ci={ci} dm={dm:?}"
                );
            }
            if dm.is_pruned() {
                assert!(
                    !any_below,
                    "pruned but a descendant matches: pattern={pattern:?} dir={dir:?} dot={dot} ci={ci}"
                );
            }
        }
    }
}

#[test]
fn dir_exhaustive_dot_matrix() {
    let pats = patterns();
    let paths = universe();
    check(&pats, &paths, false, false);
    check(&pats, &paths, true, false);
}

#[test]
fn dir_exhaustive_case_insensitive() {
    const CI_SEGMENTS: &[&str] = &["A", "b", "A*", "*A", "[A-B]", "{A,b}"];
    const CI_PSEG: &[&str] = &["a", "A", "b", "ab"];
    let mut pats = enumerate(CI_SEGMENTS, 1);
    pats.extend(enumerate(CI_SEGMENTS, 2));
    let mut paths = enumerate(CI_PSEG, 1);
    paths.extend(enumerate(CI_PSEG, 2));
    check(&pats, &paths, true, false);
    check(&pats, &paths, true, true);
}
