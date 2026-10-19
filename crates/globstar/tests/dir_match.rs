//! `match_dir` in its two roles. A pattern that selects files decides
//! whether a directory is worth walking into; a pattern that excludes
//! files decides whether a directory can be skipped whole, or needs no
//! more tests below.

use globstar::{CompileOptions, DirMatch, Glob};

fn dot(dot: bool) -> CompileOptions {
    CompileOptions::default().dot(dot)
}

fn verdict(pattern: &str, dir: &str) -> DirMatch {
    Glob::new_with(pattern, dot(true))
        .unwrap()
        .match_dir(dir.as_bytes())
}

#[test]
fn every_verdict_has_a_pattern() {
    assert_eq!(verdict("src/**/*.ts", "lib"), DirMatch::Pruned);
    assert_eq!(verdict("src", "src"), DirMatch::Match);
    assert_eq!(verdict("src/**/*.ts", "src"), DirMatch::Descend);
    assert_eq!(verdict("src{,/**/*.ts}", "src"), DirMatch::DescendAndMatch);
    assert_eq!(verdict("dist/**", "dist"), DirMatch::DescendAll);
    assert_eq!(verdict("**", "a"), DirMatch::DescendAllAndMatch);
}

#[test]
fn selecting_files_walks_into_a_directory_unless_nothing_can_match_below() {
    let select = Glob::union(["src/**/*.ts", "docs/**/*.md"]).unwrap();
    // The root is always worth walking.
    assert!(select.match_dir(b"").may_match_below());
    assert!(select.match_dir(b"src").may_match_below());
    assert!(select.match_dir(b"docs/guide").may_match_below());
    // Nothing below `node_modules` can match: skip it.
    assert_eq!(select.match_dir(b"node_modules"), DirMatch::Pruned);
    assert!(!select.match_dir(b"node_modules").may_match_below());
    // Everything below matches: every file there is a result without
    // another test.
    let all = Glob::new_with("src/**", dot(true)).unwrap();
    assert!(all.match_dir(b"src").matches_all_below());
}

#[test]
fn excluding_files_skips_a_directory_only_when_everything_below_is_excluded() {
    let exclude = Glob::union_with(["**/node_modules/**", "**/*.log"], dot(true)).unwrap();
    // Everything below is excluded: skip the directory whole.
    assert!(exclude.match_dir(b"node_modules").matches_all_below());
    assert!(exclude.match_dir(b"a/b/node_modules").matches_all_below());
    // Some files below may be excluded: walk in and test each.
    let src = exclude.match_dir(b"src");
    assert!(src.may_match_below() && !src.matches_all_below());
    // A file pattern matching the directory's own path says nothing
    // about the files below it; the directory must not be skipped.
    let logs = Glob::new_with("**/*.log", dot(true)).unwrap();
    let dir = logs.match_dir(b"x.log");
    assert_eq!(dir, DirMatch::DescendAndMatch);
    assert!(dir.matches() && !dir.matches_all_below());
    // Where nothing below can be excluded, the pattern needs no more
    // tests under the directory.
    let built = Glob::union_with(["dist/**", "build/**"], dot(true)).unwrap();
    assert_eq!(built.match_dir(b"src"), DirMatch::Pruned);
    assert_eq!(built.match_dir(b"dist"), DirMatch::DescendAll);
}

// A negated pattern is always worth walking, and says exactly whether the
// directory itself matches (§15.4). Twin of the check in dir-match.mjs.
#[test]
fn a_negated_pattern_is_always_worth_walking() {
    let g = Glob::new("!**/*.ts").unwrap();
    assert_eq!(g.match_dir(b"src"), DirMatch::DescendAndMatch);
    assert_eq!(g.match_dir(b"node_modules"), DirMatch::DescendAndMatch);
    assert_eq!(g.match_dir(b"a.ts"), DirMatch::Descend);
    for d in [&b"src"[..], b"a.ts", b"x/y.ts", b""] {
        assert_eq!(g.match_dir(d).matches(), g.is_match(d), "{d:?}");
    }
}
