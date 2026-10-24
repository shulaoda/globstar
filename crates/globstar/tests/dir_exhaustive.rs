//! Every pattern of a fixed segment alphabet (14 tokens at 1–2 segments,
//! 8 at 3) against every path of a fixed universe, for the properties that
//! define `match_dir` relative to `is_match`: the match flag is
//! `is_match(dir)`; a directory with a matching path strictly below it
//! descends; `Pruned` means no descendant matches; the anchored chains
//! and the NFA agree on every `is_match`; the all-below flag is set
//! exactly when every path up to three levels below matches (deeper than
//! any pattern, so such a path matches only through a `**`).

use globstar::{CompileOptions, DirMatch, Glob};

const SEGMENTS: &[&str] = &[
    "a", "b", "ab", "*", "?", "a*", "*a", "[ab]", "[!a]", ".a", "{a,b}", "{a,*}", "**", ".*",
];
const CORE: &[&str] = &["a", "b", "*", "?", "[ab]", ".a", "{a,b}", "**"];
/// `c` matches nothing literal in the alphabet.
const PSEG: &[&str] = &["a", "b", "ab", "c", ".a"];

/// All `/`-joined sequences of `segs` with exactly `depth` components.
fn enumerate(segs: &[&str], depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut idx = vec![0usize; depth];
    loop {
        out.push(idx.iter().map(|&i| segs[i]).collect::<Vec<_>>().join("/"));
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

/// Every path of 1..=3 `PSEG` segments, shallowest first.
fn universe() -> Vec<String> {
    let mut out = Vec::new();
    for d in 1..=3 {
        out.extend(enumerate(PSEG, d));
    }
    out
}

fn all_below(is_match: impl Fn(&[u8]) -> bool, dir: &str, suffixes: &[String]) -> bool {
    suffixes
        .iter()
        .all(|s| is_match(format!("{dir}/{s}").as_bytes()))
}

/// Every match starts with one of the static prefixes, byte for byte.
fn assert_prefixes_cover(glob: &Glob, paths: &[String], matched: &[bool]) {
    let prefixes = glob.static_prefixes();
    for (path, _) in paths.iter().zip(matched).filter(|(_, m)| **m) {
        let p = path.as_bytes();
        assert!(
            prefixes
                .iter()
                .any(|q| q.is_empty() || p == &q[..] || (p.starts_with(q) && p[q.len()] == b'/')),
            "{glob:?}: {path:?} starts with none of {prefixes:?}"
        );
    }
}

fn is_below(child: &str, dir: &str) -> bool {
    child.len() > dir.len() && child.as_bytes()[dir.len()] == b'/' && child.starts_with(dir)
}

fn below_map(paths: &[String]) -> Vec<Vec<usize>> {
    paths
        .iter()
        .map(|d| {
            paths
                .iter()
                .enumerate()
                .filter(|(_, p)| is_below(p, d))
                .map(|(j, _)| j)
                .collect()
        })
        .collect()
}

fn check(patterns: &[String], paths: &[String], dot: bool, ci: bool) {
    check_below(patterns, paths, &universe(), dot, ci);
}

/// All-below judged on `dir/s` for each of `suffixes`.
fn check_below(patterns: &[String], paths: &[String], suffixes: &[String], dot: bool, ci: bool) {
    let below = below_map(paths);
    for pattern in patterns {
        let opts = CompileOptions::default().dot(dot).case_insensitive(ci);
        let glob = Glob::new_with(pattern, opts).expect("compile");
        let nfa = Glob::new_with(pattern, opts.force_nfa(true)).expect("compile");
        check_glob(pattern, [&glob, &nfa], paths, suffixes, &below, dot, ci);
    }
}

fn check_union(sets: &[Vec<&'static str>], paths: &[String], dot: bool, ci: bool) {
    let below = below_map(paths);
    let suffixes = universe();
    for set in sets {
        let opts = CompileOptions::default().dot(dot).case_insensitive(ci);
        let glob = Glob::union_with(set.iter().copied(), opts).expect("union");
        let nfa = Glob::union_with(set.iter().copied(), opts.force_nfa(true)).expect("union");
        check_glob(set, [&glob, &nfa], paths, &suffixes, &below, dot, ci);
    }
}

fn check_glob(
    label: &dyn std::fmt::Debug,
    [glob, nfa]: [&Glob; 2],
    paths: &[String],
    suffixes: &[String],
    below: &[Vec<usize>],
    dot: bool,
    ci: bool,
) {
    let matched: Vec<bool> = paths.iter().map(|p| glob.is_match(p.as_bytes())).collect();
    // Under case_insensitive a match may differ in case from its prefix.
    if !ci {
        assert_prefixes_cover(glob, paths, &matched);
    }
    for (i, dir) in paths.iter().enumerate() {
        let ctx = format!("{label:?} dir={dir:?} dot={dot} ci={ci}");
        let nfa = nfa.is_match(dir.as_bytes());
        assert_eq!(
            matched[i], nfa,
            "is_match disagreement: {ctx} chains={} nfa={nfa}",
            matched[i]
        );

        let dm = glob.match_dir(dir.as_bytes());
        assert_eq!(
            dm.matches(),
            matched[i],
            "match flag != is_match: {ctx} dm={dm:?}"
        );

        let any_below = below[i].iter().any(|&j| matched[j]);
        if any_below {
            assert!(
                dm.may_match_below(),
                "walker would lose a match: {ctx} dm={dm:?}"
            );
        }
        if dm == DirMatch::Pruned {
            assert!(!any_below, "pruned but a descendant matches: {ctx}");
        }

        assert_eq!(
            dm.matches_all_below(),
            all_below(|p| glob.is_match(p), dir, suffixes),
            "all-below flag != every descendant matches: {ctx} dm={dm:?}"
        );
    }
}

#[test]
fn dir_exhaustive_dot_matrix() {
    let pats = patterns();
    let paths = universe();
    check(&pats, &paths, false, false);
    check(&pats, &paths, true, false);
}

/// Every ordered pair of eight patterns, every ordered triple of four.
#[test]
fn dir_exhaustive_unions() {
    const UNION_PATTERNS: &[&str] = &["a/b", "a/*", "*/b", "**/b", "a/**", "{a,b}/c", ".a/*", "?"];
    const UNION_CORE: &[&str] = &["a/*", "**/b", ".a/*", "?"];

    let mut sets: Vec<Vec<&'static str>> = Vec::new();
    for &a in UNION_PATTERNS {
        for &b in UNION_PATTERNS {
            sets.push(vec![a, b]);
        }
    }
    for &a in UNION_CORE {
        for &b in UNION_CORE {
            for &c in UNION_CORE {
                sets.push(vec![a, b, c]);
            }
        }
    }

    let paths = universe();
    check_union(&sets, &paths, false, false);
    check_union(&sets, &paths, true, false);
}

/// A globstar, then three elements: the chain's tail tokens take one or
/// two of them. Paths go four deep, all-below three.
#[test]
fn dir_exhaustive_long_tails() {
    const HEADS: &[&str] = &["", "a/", "*/"];
    const TAIL: &[&str] = &["*", "a", "?", ".a"];
    const SEG: &[&str] = &["a", "b", ".a"];
    let pats: Vec<String> = HEADS
        .iter()
        .flat_map(|h| {
            enumerate(TAIL, 3)
                .into_iter()
                .map(move |t| format!("{h}**/{t}"))
        })
        .collect();
    let paths: Vec<String> = (1..=4).flat_map(|d| enumerate(SEG, d)).collect();
    let suffixes: Vec<String> = (1..=3).flat_map(|d| enumerate(SEG, d)).collect();
    check_below(&pats, &paths, &suffixes, false, false);
    check_below(&pats, &paths, &suffixes, true, false);
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
