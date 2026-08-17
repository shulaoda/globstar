//! Integration tests for [`Walk`] against real temporary directory trees.
//!
//! Only the public API is exercised (no access to private internals).

use globstar_walk::{Walk, WalkError, WalkOptions};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Minimal self-cleaning temporary directory.
struct TmpTree {
    root: PathBuf,
}

impl TmpTree {
    fn new(tag: &str) -> Self {
        let mut root = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        root.push(format!("glob-walker-test-{tag}-{pid}-{nanos}"));
        fs::create_dir_all(&root).expect("create tmp root");
        Self { root }
    }

    fn touch(&self, rel: &str) {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::File::create(&p).expect("create file");
    }

    fn mkdir(&self, rel: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(&p).expect("create dir");
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for TmpTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn collect_rel(walker: Walk, root: &Path) -> BTreeSet<String> {
    walker
        .map(|r| r.expect("no walker error in test").into_path())
        .map(|p| {
            p.strip_prefix(root)
                .expect("strip root prefix")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn walker_literal_direct_hit() {
    let t = TmpTree::new("literal");
    t.touch("src/main.rs");
    t.touch("src/lib.rs");
    t.touch("README.md");

    let got = collect_rel(Walk::new("src/main.rs", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/main.rs"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_star_extension_prunes_other_dirs() {
    let t = TmpTree::new("starext");
    t.touch("src/main.rs");
    t.touch("src/lib.rs");
    t.touch("src/notes.md");
    t.touch("tests/smoke.rs");

    let got = collect_rel(Walk::new("src/*.rs", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/lib.rs", "src/main.rs"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_globstar_recursive() {
    let t = TmpTree::new("globstar");
    t.touch("src/main.rs");
    t.touch("src/cli/run.rs");
    t.touch("src/cli/commands/build.rs");
    t.touch("src/notes.md");
    t.touch("tests/smoke.rs");

    let got = collect_rel(Walk::new("src/**/*.rs", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/cli/commands/build.rs", "src/cli/run.rs", "src/main.rs"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_brace_expansion() {
    let t = TmpTree::new("brace");
    t.touch("a.ts");
    t.touch("b.tsx");
    t.touch("c.js");
    t.touch("d.rs");

    let got = collect_rel(Walk::new("*.{ts,tsx}", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["a.ts", "b.tsx"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_extglob_negate() {
    // `(*.min).js` is parsed as literal `(` + Star + literal `.min).js`
    // (no extglob support — `!(...)` is NOT bash extglob in this engine).
    // None of the files is literally `(<anything>.min).js`, so an ignore
    // pattern of that shape removes nothing from the walk result.
    //
    // The walker auto-splits leading-`!` patterns into the ignore set,
    // so passing `["**/*.js", "!(*.min).js"]` is the same as
    // `["**/*.js"]` + `ignore: ["(*.min).js"]`.
    let t = TmpTree::new("extneg");
    t.touch("main.js");
    t.touch("main.min.js");
    t.touch("util.js");
    t.touch("util.min.js");

    let got = collect_rel(
        Walk::from_patterns(["**/*.js", "!(*.min).js"], t.root()).unwrap(),
        t.root(),
    );
    let expected: BTreeSet<String> = ["main.js", "main.min.js", "util.js", "util.min.js"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_lone_negation_yields_nothing() {
    // A lone `!`-prefixed pattern means "no positive patterns" after the
    // auto-split — so the walker yields nothing. Replaces the previous
    // GlobSet-aggregated "everything-except-X" footgun semantic
    // (cf. picomatch array of negated patterns).
    let t = TmpTree::new("lone_neg");
    t.touch("a.js");
    t.touch("b.js");

    let got = collect_rel(Walk::new("!a.js", t.root()).unwrap(), t.root());
    assert!(got.is_empty(), "expected empty, got {got:?}");
}

#[test]
fn walker_multi_bang_parity_routes_by_count() {
    // GLOB_SPEC §9.5: leading-`!` parity rule. Odd count → ignore set,
    // even count → match set; all leading `!`s are stripped from the
    // body. JS walker uses the same rule (`walker/walk.js`).
    let t = TmpTree::new("multi_bang");
    t.touch("a.js");
    t.touch("b.js");

    // `!!a.js` (2 bangs, even) → positive `a.js`. Yields a.js only.
    let got = collect_rel(Walk::from_patterns(["!!a.js"], t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["a.js"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected, "!!a.js should resolve to positive a.js");

    // `**/*.js` + `!!!a.js` (3 bangs, odd) → ignore `a.js`. Yields b.js only.
    let got = collect_rel(
        Walk::from_patterns(["**/*.js", "!!!a.js"], t.root()).unwrap(),
        t.root(),
    );
    let expected: BTreeSet<String> = ["b.js"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected, "!!!a.js should route a.js into ignore set");
}

#[test]
fn walker_prunes_unrelated_subtrees() {
    let t = TmpTree::new("prune");
    t.touch("src/a.ts");
    t.touch("src/b.ts");
    t.touch("other/c.ts");
    t.touch("other/deep/d.ts");

    let got = collect_rel(Walk::new("src/**/*.ts", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/a.ts", "src/b.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_empty_dir() {
    let t = TmpTree::new("empty");
    let got = collect_rel(Walk::new("**/*.rs", t.root()).unwrap(), t.root());
    assert!(got.is_empty());
}

#[test]
fn walker_yields_dir_match() {
    let t = TmpTree::new("dirmatch");
    t.mkdir("src/components");
    t.touch("src/components/button.ts");
    t.touch("src/other.ts");

    let got = collect_rel(Walk::new("src/components", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/components"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_dot_protection_default_hides_dotfiles() {
    let t = TmpTree::new("dotprot");
    t.touch(".env");
    t.touch("main.rs");

    let got = collect_rel(Walk::new("*", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["main.rs"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_dot_true_includes_dotfiles() {
    let t = TmpTree::new("dot_true");
    t.touch(".env");
    t.touch("main.rs");

    let walker = Walk::new(
        "*",
        WalkOptions {
            dot: true,
            ..WalkOptions::new(t.root())
        },
    )
    .unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = [".env", "main.rs"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_case_sensitive_default_misses_other_case() {
    let t = TmpTree::new("case_strict");
    t.touch("Main.TS");
    t.touch("other.rs");

    let got = collect_rel(Walk::new("*.ts", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = BTreeSet::new();
    assert_eq!(
        got, expected,
        "case-sensitive default should not match Main.TS"
    );
}

#[test]
fn walker_case_insensitive_matches_mixed_case() {
    let t = TmpTree::new("case_insens");
    t.touch("Main.TS");
    t.touch("LIB.ts");
    t.touch("other.rs");

    let walker = Walk::new(
        "*.ts",
        WalkOptions {
            case_insensitive: true,
            ..WalkOptions::new(t.root())
        },
    )
    .unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["Main.TS", "LIB.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_missing_prefix_yields_nothing_silently() {
    let t = TmpTree::new("missing");
    t.touch("unrelated.rs");

    let got = collect_rel(
        Walk::new("nonexistent/**/*.rs", t.root()).unwrap(),
        t.root(),
    );
    assert!(got.is_empty());
}

#[test]
fn walker_prefix_skips_sibling_dirs() {
    let t = TmpTree::new("prefixskip");
    t.touch("src/main.ts");
    t.touch("src/sub/foo.ts");
    t.touch("node_modules/a.ts");
    t.touch("node_modules/deep/nested/b.ts");

    let got = collect_rel(Walk::new("src/**/*.ts", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/main.ts", "src/sub/foo.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_brace_prefix_walks_both_roots() {
    let t = TmpTree::new("braceroots");
    t.touch("src/main.rs");
    t.touch("tests/smoke.rs");
    t.touch("lib/vendored.rs");

    let got = collect_rel(Walk::new("{src,tests}/*.rs", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/main.rs", "tests/smoke.rs"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_pure_literal_missing_file_yields_nothing() {
    let t = TmpTree::new("pureliteralmiss");
    t.touch("other.rs");

    let got = collect_rel(Walk::new("src/main.rs", t.root()).unwrap(), t.root());
    assert!(got.is_empty());
}

#[test]
fn walker_from_patterns_union() {
    let t = TmpTree::new("multi_union");
    t.touch("src/main.ts");
    t.touch("src/lib.rs");
    t.touch("README.md");
    t.touch("tests/smoke.ts");

    let walker = Walk::from_patterns(["README.md", "src/*.rs", "**/*.ts"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["README.md", "src/lib.rs", "src/main.ts", "tests/smoke.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_from_patterns_shared_prefix() {
    let t = TmpTree::new("multi_shared_prefix");
    t.touch("src/main.ts");
    t.touch("src/lib.rs");
    t.touch("src/notes.md");
    t.touch("dist/bundle.js");
    t.touch("node_modules/pkg/index.ts");

    let walker = Walk::from_patterns(["src/**/*.ts", "src/**/*.rs"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["src/lib.rs", "src/main.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_from_patterns_distinct_prefixes() {
    let t = TmpTree::new("multi_disjoint");
    t.touch("src/main.rs");
    t.touch("tests/smoke.rs");
    t.touch("lib/vendored.rs");

    let walker = Walk::from_patterns(["src/*.rs", "tests/*.rs"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["src/main.rs", "tests/smoke.rs"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_from_patterns_pruning_requires_all_agree() {
    let t = TmpTree::new("multi_coop_pruning");
    t.touch("src/main.ts");
    t.touch("node_modules/pkg/package.json");

    let walker =
        Walk::from_patterns(["src/**/*.ts", "node_modules/**/package.json"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["node_modules/pkg/package.json", "src/main.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_from_patterns_literal_file_in_union() {
    let t = TmpTree::new("multi_literal_file");
    t.touch("src/main.ts");
    t.touch("package.json");

    let walker = Walk::from_patterns(["package.json", "src/**/*.ts"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["package.json", "src/main.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_from_patterns_empty_yields_nothing() {
    let t = TmpTree::new("multi_empty");
    t.touch("main.rs");

    let walker = Walk::from_patterns::<[&str; 0], &str>([], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    assert!(got.is_empty());
}

#[test]
fn walker_ignore_skips_files() {
    let t = TmpTree::new("ignore_files");
    t.touch("src/main.rs");
    t.touch("src/gen.rs");
    t.touch("src/lib.rs");

    let walker = Walk::new(
        "src/*.rs",
        WalkOptions {
            ignore: vec!["**/gen.rs".into()],
            ..WalkOptions::new(t.root())
        },
    )
    .unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["src/lib.rs", "src/main.rs"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_ignore_prunes_dirs() {
    let t = TmpTree::new("ignore_prune");
    t.touch("src/a.ts");
    t.touch("src/deep/b.ts");
    t.touch("node_modules/pkg/index.ts");
    t.touch("node_modules/pkg/nested/x.ts");

    let walker = Walk::new(
        "**/*.ts",
        WalkOptions {
            ignore: vec!["**/node_modules/**".into()],
            ..WalkOptions::new(t.root())
        },
    )
    .unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["src/a.ts", "src/deep/b.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_new_invalid_pattern_returns_err() {
    let t = TmpTree::new("new_bad");
    let err = Walk::new("[unclosed", t.root()).unwrap_err();
    match err {
        WalkError::InvalidPattern { pattern, .. } => assert_eq!(pattern, "[unclosed"),
        _ => panic!("expected InvalidPattern, got {err:?}"),
    }
}

#[test]
fn walker_ignore_invalid_pattern_returns_err() {
    let t = TmpTree::new("ignore_bad");
    t.touch("a.rs");
    let err = Walk::new(
        "*.rs",
        WalkOptions {
            ignore: vec!["[bad".into()],
            ..WalkOptions::new(t.root())
        },
    )
    .unwrap_err();
    match err {
        WalkError::InvalidPattern { pattern, .. } => assert_eq!(pattern, "[bad"),
        _ => panic!("expected InvalidPattern, got {err:?}"),
    }
}

#[test]
fn walker_rust_workspace_finds_source_files() {
    let t = TmpTree::new("rust_workspace");
    t.touch("Cargo.toml");
    t.touch("Cargo.lock");
    t.touch("crates/core/Cargo.toml");
    t.touch("crates/core/src/lib.rs");
    t.touch("crates/core/src/parser.rs");
    t.touch("crates/core/src/engine/mod.rs");
    t.touch("crates/core/src/engine/backtrack.rs");
    t.touch("crates/core/tests/integration.rs");
    t.touch("crates/cli/Cargo.toml");
    t.touch("crates/cli/src/main.rs");
    t.touch("crates/cli/src/args.rs");
    t.touch("target/debug/deps/core-abc.rlib");
    t.touch("target/debug/build/core-abc/build-script-build");
    t.touch("target/release/cli.exe");

    let got = collect_rel(
        Walk::new("crates/*/src/**/*.rs", t.root()).unwrap(),
        t.root(),
    );
    let expected: BTreeSet<String> = [
        "crates/cli/src/args.rs",
        "crates/cli/src/main.rs",
        "crates/core/src/engine/backtrack.rs",
        "crates/core/src/engine/mod.rs",
        "crates/core/src/lib.rs",
        "crates/core/src/parser.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_rust_workspace_multi_source_and_tests() {
    let t = TmpTree::new("rust_ws_set");
    t.touch("crates/core/src/lib.rs");
    t.touch("crates/core/tests/integration.rs");
    t.touch("crates/cli/src/main.rs");
    t.touch("crates/cli/tests/smoke.rs");
    t.touch("target/debug/deps/core.rlib");

    let walker =
        Walk::from_patterns(["crates/*/src/**/*.rs", "crates/*/tests/**/*.rs"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = [
        "crates/cli/src/main.rs",
        "crates/cli/tests/smoke.rs",
        "crates/core/src/lib.rs",
        "crates/core/tests/integration.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_node_project_typescript_sources() {
    let t = TmpTree::new("node_project");
    t.touch("package.json");
    t.touch("tsconfig.json");
    t.touch("src/index.ts");
    t.touch("src/components/Button.tsx");
    t.touch("src/components/Modal.tsx");
    t.touch("src/utils/math.ts");
    t.touch("src/utils/strings.ts");
    t.touch("tests/Button.test.tsx");
    t.touch("dist/index.js");
    t.touch("dist/index.d.ts");
    t.touch("node_modules/react/index.js");
    t.touch("node_modules/react/index.d.ts");
    t.touch("node_modules/lodash/src/index.ts");

    let got = collect_rel(Walk::new("src/**/*.{ts,tsx}", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = [
        "src/components/Button.tsx",
        "src/components/Modal.tsx",
        "src/index.ts",
        "src/utils/math.ts",
        "src/utils/strings.ts",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_node_project_sources_and_tests() {
    let t = TmpTree::new("node_src_and_tests");
    t.touch("src/Button.tsx");
    t.touch("src/Button.test.tsx");
    t.touch("src/utils.ts");
    t.touch("tests/integration/auth.test.ts");
    t.touch("node_modules/react/index.js");

    let walker =
        Walk::from_patterns(["src/**/*.{ts,tsx}", "tests/**/*.test.{ts,tsx}"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = [
        "src/Button.test.tsx",
        "src/Button.tsx",
        "src/utils.ts",
        "tests/integration/auth.test.ts",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_python_project_source_files() {
    let t = TmpTree::new("python_project");
    t.touch("setup.py");
    t.touch("README.md");
    t.touch("src/mypackage/__init__.py");
    t.touch("src/mypackage/core.py");
    t.touch("src/mypackage/utils/helpers.py");
    t.touch("src/mypackage/__pycache__/__init__.cpython-311.pyc");
    t.touch("src/mypackage/__pycache__/core.cpython-311.pyc");
    t.touch(".venv/bin/python");
    t.touch(".venv/lib/python3.11/site-packages/pip/__init__.py");

    let got = collect_rel(Walk::new("src/**/*.py", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = [
        "src/mypackage/__init__.py",
        "src/mypackage/core.py",
        "src/mypackage/utils/helpers.py",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_wide_flat_directory() {
    let t = TmpTree::new("wide_flat");
    for i in 0..100 {
        t.touch(&format!("files/item_{i}.txt"));
    }
    t.touch("files/README.md");
    t.touch("files/config.json");

    let got = collect_rel(Walk::new("files/*.txt", t.root()).unwrap(), t.root());
    assert_eq!(got.len(), 100);
    assert!(got.contains("files/item_0.txt"));
    assert!(got.contains("files/item_50.txt"));
    assert!(got.contains("files/item_99.txt"));
    assert!(!got.contains("files/README.md"));
    assert!(!got.contains("files/config.json"));
}

#[test]
fn walker_deep_nesting() {
    let t = TmpTree::new("deep_nest");
    t.touch("a/b/c/d/e/f/g/h/leaf.rs");
    t.touch("a/b/c/d/shallow.rs");
    t.touch("a/shallow.rs");
    t.touch("unrelated.rs");

    let got = collect_rel(Walk::new("a/**/*.rs", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = [
        "a/b/c/d/e/f/g/h/leaf.rs",
        "a/b/c/d/shallow.rs",
        "a/shallow.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_recursive_suffix_pattern() {
    let t = TmpTree::new("recursive_suffix");
    t.touch("dist/bundle.min.js");
    t.touch("dist/bundle.js");
    t.touch("vendor/jquery.min.js");
    t.touch("vendor/jquery.js");
    t.touch("src/main.ts");
    t.touch("public/assets/analytics.min.js");

    let got = collect_rel(Walk::new("**/*.min.js", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = [
        "dist/bundle.min.js",
        "public/assets/analytics.min.js",
        "vendor/jquery.min.js",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_extglob_non_min_js() {
    // `src/!(*.min).js` → `src/` + literal `!(` + Star + literal
    // `.min).js`. `!` here isn't leading, so it's a literal byte; the
    // pattern matches basenames of the shape `!(<anything>.min).js`.
    let t = TmpTree::new("non_min_js");
    t.touch("src/main.js");
    t.touch("src/utils.js");
    t.touch("dist/bundle.min.js");
    t.touch("dist/app.min.js");
    t.touch("vendor/lib.js");
    t.touch("README.md");
    t.touch("src/!(bundle.min).js");

    let got = collect_rel(Walk::new("src/!(*.min).js", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["src/!(bundle.min).js"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_literal_directory_target() {
    let t = TmpTree::new("literal_target");
    t.touch("target/debug/main");
    t.touch("target/release/main");
    t.touch("src/main.rs");

    let got = collect_rel(Walk::new("target", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["target"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_mixed_tiers_via_from_patterns() {
    let t = TmpTree::new("mixed_tiers");
    t.touch("Cargo.toml");
    t.touch("Cargo.lock");
    t.touch("README.md");
    t.touch("src/lib.rs");
    t.touch("src/cli/main.rs");
    t.touch("tests/smoke.rs");
    t.touch("target/debug/deps/crate.rlib");

    let walker = Walk::from_patterns(["Cargo.toml", "*.md", "src/**/*.rs"], t.root()).unwrap();
    let got = collect_rel(walker, t.root());
    let expected: BTreeSet<String> = ["Cargo.toml", "README.md", "src/cli/main.rs", "src/lib.rs"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

/// Serializes tests that mutate process-wide CWD so they don't race
/// against each other. The rest of this file uses absolute paths from
/// `TmpTree::root()` and never touches CWD, so they don't need this guard.
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn walker_default_options_walks_cwd() {
    let _guard = CWD_LOCK.lock().expect("cwd lock poisoned");
    let t = TmpTree::new("default_cwd");
    t.touch("a.rs");
    t.touch("b.rs");
    t.touch("c.md");

    let orig = std::env::current_dir().expect("save cwd");
    std::env::set_current_dir(t.root()).expect("chdir into tmp");

    // WalkOptions::default() → base = "." → should walk the current dir.
    // Collect file names (paths are yielded as `./a.rs` etc, so strip to
    // basenames for assertion).
    let files: BTreeSet<String> = Walk::new("*.rs", WalkOptions::default())
        .expect("construct walker")
        .map(|r| r.expect("no walker error in test"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    std::env::set_current_dir(&orig).expect("restore cwd");

    let expected: BTreeSet<String> = ["a.rs", "b.rs"].iter().map(|s| s.to_string()).collect();
    assert_eq!(files, expected);
}

#[test]
fn walker_base_locked_to_construct_time_cwd() {
    // Proves `std::path::absolute`-based base locking: changing CWD after
    // construction does NOT redirect the walker to a different directory.
    let _guard = CWD_LOCK.lock().expect("cwd lock poisoned");
    let t1 = TmpTree::new("locked_t1");
    t1.touch("only_in_t1.rs");
    let t2 = TmpTree::new("locked_t2");
    t2.touch("only_in_t2.rs");

    let orig = std::env::current_dir().expect("save cwd");
    std::env::set_current_dir(t1.root()).expect("chdir into t1");

    // Construct while CWD == t1.
    let walker = Walk::new("*.rs", WalkOptions::default()).expect("construct walker");

    // Move CWD to t2 BEFORE iterating — with absolute-base lock-in, the
    // walker should still walk t1 (not t2) because its base was snapshotted.
    std::env::set_current_dir(t2.root()).expect("chdir into t2");

    let files: BTreeSet<String> = walker
        .map(|r| r.expect("no walker error in test"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    std::env::set_current_dir(&orig).expect("restore cwd");

    let expected: BTreeSet<String> = ["only_in_t1.rs"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        files, expected,
        "walker should be locked to t1 (its construct-time CWD), not t2"
    );
}

#[test]
fn walker_relative_base_yields_absolute_paths() {
    // A relative input base (here `.` via WalkOptions::default()) should be
    // absolutized at construction, so yielded paths come back absolute.
    let _guard = CWD_LOCK.lock().expect("cwd lock poisoned");
    let t = TmpTree::new("rel_base_abs_out");
    t.touch("foo.rs");

    let orig = std::env::current_dir().expect("save cwd");
    std::env::set_current_dir(t.root()).expect("chdir into tmp");

    let paths: Vec<PathBuf> = Walk::new("*.rs", WalkOptions::default())
        .expect("construct walker")
        .map(|r| r.expect("no walker error in test").into_path())
        .collect();

    std::env::set_current_dir(&orig).expect("restore cwd");

    assert_eq!(paths.len(), 1, "expected exactly one yielded path");
    assert!(
        paths[0].is_absolute(),
        "walker with relative input base must yield absolute paths, got: {:?}",
        paths[0]
    );
}

#[test]
fn walker_leading_slash_stays_within_base() {
    // Security invariant: a pattern with a leading `/` must NOT let the
    // walker escape `opts.base`. Library callers that forward untrusted
    // user input as patterns rely on `opts.base` being a real sandbox;
    // silently propagating `Path::join`'s "absolute replaces base" would
    // be a path-traversal class of bug.
    let t_sandbox = TmpTree::new("leading_slash_sandbox");
    t_sandbox.touch("x.ts");
    let t_escape = TmpTree::new("leading_slash_escape_target");
    t_escape.touch("escape_me.ts");

    let pattern = format!("{}/*.ts", t_escape.root().display());
    let got = collect_rel(
        Walk::new(&pattern, t_sandbox.root()).expect("construct walker"),
        t_sandbox.root(),
    );

    assert!(
        got.is_empty(),
        "leading `/` in pattern must not let the walker escape opts.base; \
         escape target's files must not be yielded. got: {got:?}"
    );
}

#[test]
fn dir_entry_file_type_distinguishes_file_from_dir() {
    let t = TmpTree::new("ft_file_vs_dir");
    t.touch("readme.md");
    t.mkdir("docs");
    t.touch("docs/intro.md");

    // Match both: files at any depth and the `docs` dir itself.
    let mut files = 0u32;
    let mut dirs = 0u32;
    for r in Walk::from_patterns(["**/*.md", "docs"], t.root()).unwrap() {
        let ft = r.expect("no walker error").file_type();
        if ft.is_file() {
            files += 1;
        } else if ft.is_dir() {
            dirs += 1;
        }
    }
    assert_eq!(files, 2, "expected 2 files (readme.md, docs/intro.md)");
    assert_eq!(dirs, 1, "expected 1 dir (docs)");
}

#[test]
fn dir_entry_depth_tracks_descent() {
    let t = TmpTree::new("depth");
    t.touch("a.rs"); // depth 1
    t.touch("b/c.rs"); // depth 2
    t.touch("b/d/e.rs"); // depth 3

    let mut depths: Vec<usize> = Walk::new("**/*.rs", t.root())
        .unwrap()
        .map(|r| r.expect("no walker error").depth())
        .collect();
    depths.sort_unstable();
    assert_eq!(depths, vec![1, 2, 3]);
}

#[test]
fn dir_entry_file_name_returns_leaf() {
    let t = TmpTree::new("leaf_name");
    t.touch("nested/deeply/target.txt");

    let entry = Walk::new("**/target.txt", t.root())
        .unwrap()
        .next()
        .expect("at least one entry")
        .expect("no walker error");

    assert_eq!(entry.file_name(), "target.txt");
}

#[test]
fn dir_entry_metadata_succeeds_for_regular_file() {
    // Mostly a smoke test: the Windows code path caches metadata from
    // readdir, the Unix path does a fresh symlink_metadata — both should
    // succeed for a plain file, and the file size should be recorded.
    let t = TmpTree::new("meta_smoke");
    let path = t.root().join("hello.txt");
    fs::write(&path, b"hi there").expect("write file");

    let entry = Walk::new("*.txt", t.root())
        .unwrap()
        .next()
        .expect("at least one entry")
        .expect("no walker error");

    let md = entry.metadata().expect("metadata ok");
    assert!(md.is_file());
    assert_eq!(md.len(), b"hi there".len() as u64);
}

#[cfg(unix)]
#[test]
fn walker_follow_links_off_drops_symlinks() {
    // Aligns with tinyglobby's `excludeSymlinks` semantics: when
    // `follow_links: false`, symlinks are dropped entirely (not
    // emitted, not descended). Default is `true`, which is what the
    // rest of the test corpus exercises.
    use std::os::unix::fs::symlink;
    let t = TmpTree::new("follow_off");
    t.touch("real.txt");
    symlink(t.root().join("real.txt"), t.root().join("link.txt")).expect("create file symlink");

    let got = collect_rel(
        Walk::new(
            "*.txt",
            WalkOptions {
                follow_links: false,
                ..WalkOptions::new(t.root())
            },
        )
        .unwrap(),
        t.root(),
    );
    // Only the real file — symlink dropped.
    let want: BTreeSet<String> = std::iter::once("real.txt".to_string()).collect();
    assert_eq!(
        got, want,
        "follow_links:false should drop symlinks entirely"
    );
}

#[cfg(unix)]
#[test]
fn walker_follow_links_on_breaks_cycles() {
    // A directory symlink that loops back to its own ancestor would
    // cause infinite descent without cycle detection. Walker must
    // canonicalize the symlink target, see it's an ancestor, skip
    // the descent, and produce a finite result.
    use std::os::unix::fs::symlink;
    let t = TmpTree::new("follow_cycle");
    t.touch("a.txt");
    // `t/loop` → `t` (the parent directory). Walking `t` would
    // visit `t/loop`, which IS `t`, which contains `loop` again …
    symlink(t.root(), t.root().join("loop")).expect("create dir-cycle symlink");

    let got = collect_rel(
        Walk::new(
            "**/*.txt",
            WalkOptions {
                follow_links: true,
                ..WalkOptions::new(t.root())
            },
        )
        .unwrap(),
        t.root(),
    );
    // Walker terminates. Without cycle detection it would loop
    // forever; with it, descent into `loop` is suppressed at the
    // first re-entry and we get the single real file (plus possibly
    // the same file under one level of `loop/` before the cycle is
    // tripped, depending on exact resolution timing — assert the
    // real file is in the set and the result is finite).
    assert!(
        got.contains("a.txt"),
        "cycle protection must still yield the real file; got {got:?}"
    );
    assert!(
        got.iter().all(|p| !p.contains("loop/loop")),
        "cycle protection must cap depth at one re-entry; got {got:?}"
    );
}

// ── Regression: audit 2026-08 ───────────────────────────────────────────

/// C1: a union of literal patterns that share a trailing byte (e.g. the
/// same extension) must not seed the walker at the un-terminated prefix
/// (`package`/`tsconfig`) — that directory doesn't exist, so the walk
/// used to return nothing.
#[test]
fn walker_c1_multi_literal_shared_suffix() {
    let t = TmpTree::new("c1-multi-lit");
    t.touch("package.json");
    t.touch("tsconfig.json");
    t.touch("readme.md");
    let got = collect_rel(
        Walk::from_patterns(["package.json", "tsconfig.json"], t.root()).unwrap(),
        t.root(),
    );
    let expected: BTreeSet<String> = ["package.json", "tsconfig.json"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

/// C1: same hazard via a leading brace whose branches share a suffix.
#[test]
fn walker_c1_brace_shared_suffix() {
    let t = TmpTree::new("c1-brace");
    t.touch("index.ts");
    t.touch("main.ts");
    t.touch("other.ts");
    let got = collect_rel(Walk::new("{index,main}.ts", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["index.ts", "main.ts"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, expected);
}

/// C2: a `..` in a pattern must not let the walk escape `base`, even
/// though the sibling directory exists on disk.
#[test]
fn walker_c2_dotdot_stays_in_base() {
    let outer = TmpTree::new("c2-outer");
    outer.mkdir("sandbox");
    outer.touch("secret/passwd.txt");
    outer.touch("sandbox/ok.txt");
    let base = outer.root().join("sandbox");

    for pat in ["../secret/*.txt", "../**/*.txt", "a/../../secret/*.txt"] {
        let got = collect_rel(Walk::new(pat, base.clone()).unwrap(), &base);
        assert!(
            got.is_empty(),
            "pattern {pat:?} escaped the sandbox: {got:?}"
        );
    }
    // A normal in-base pattern still works.
    let got = collect_rel(Walk::new("*.txt", base.clone()).unwrap(), &base);
    assert_eq!(got, ["ok.txt"].iter().map(|s| s.to_string()).collect());
}

/// Seed-jump must honor `follow_links = false`: a static-prefix that
/// lies on or beyond a symlink is never reached by a root walk (which
/// drops symlinks), so seeding must not jump through it either.
#[test]
#[cfg(unix)]
fn walker_seed_symlink_follow_false() {
    use std::os::unix::fs::symlink;
    let t = TmpTree::new("seed-symlink");
    t.touch("real/a.txt");
    symlink(t.root().join("real"), t.root().join("link")).unwrap();
    symlink(t.root().join("real/a.txt"), t.root().join("flink.txt")).unwrap();

    let opts = |follow: bool| WalkOptions {
        base: t.root().to_path_buf(),
        follow_links: follow,
        ..Default::default()
    };

    // follow = false: seeding through the symlinked prefix is suppressed.
    for pat in ["link/*.txt", "flink.txt"] {
        let got = collect_rel(Walk::new(pat, opts(false)).unwrap(), t.root());
        assert!(
            got.is_empty(),
            "follow=false seeded through symlink {pat:?}: {got:?}"
        );
    }
    // A root walk (empty prefix) reaches only the real file, not the links.
    let got = collect_rel(Walk::new("**/*.txt", opts(false)).unwrap(), t.root());
    assert_eq!(got, ["real/a.txt"].iter().map(|s| s.to_string()).collect());

    // follow = true: the seed IS allowed to jump through the symlink.
    let got = collect_rel(Walk::new("link/*.txt", opts(true)).unwrap(), t.root());
    assert_eq!(got, ["link/a.txt"].iter().map(|s| s.to_string()).collect());
}

// ── 2026-08 audit regressions ───────────────────────────────────────────

#[test]
fn walker_ignore_applies_to_seeded_prefixes() {
    // Pinned 2026-08: static-prefix seeds (file emissions AND dir
    // frames) skipped the ignore filter entirely, so fully-literal
    // patterns escaped `ignore` while glob-shaped ones honored it.
    let t = TmpTree::new("ignore_seed");
    t.touch("src/generated/api.ts");
    t.touch("src/index.ts");

    let opts = |ignore: &[&str]| WalkOptions {
        ignore: ignore.iter().map(|s| s.to_string()).collect(),
        ..WalkOptions::new(t.root())
    };

    // Seeded literal FILE.
    let got = collect_rel(
        Walk::new("src/generated/api.ts", opts(&["**/generated/**"])).unwrap(),
        t.root(),
    );
    assert!(got.is_empty(), "seeded file escaped ignore: {got:?}");

    // §4.2 equivalence: the auto-split negative must behave identically.
    let got = collect_rel(
        Walk::from_patterns(["src/generated/api.ts", "!**/generated/**"], t.root()).unwrap(),
        t.root(),
    );
    assert!(
        got.is_empty(),
        "auto-split negative escaped seeding: {got:?}"
    );

    // Seeded DIR whose own relative path is ignored.
    let got = collect_rel(
        Walk::new("src/generated/*.ts", opts(&["**/generated"])).unwrap(),
        t.root(),
    );
    assert!(got.is_empty(), "seeded dir escaped ignore: {got:?}");

    // Seeded prefix under an ignored ANCESTOR directory.
    let got = collect_rel(
        Walk::new("src/generated/api.ts", opts(&["src"])).unwrap(),
        t.root(),
    );
    assert!(
        got.is_empty(),
        "seed under ignored ancestor escaped: {got:?}"
    );

    // Control: the non-seeded glob form agrees.
    let got = collect_rel(
        Walk::new("src/**/*.ts", opts(&["**/generated/**"])).unwrap(),
        t.root(),
    );
    let expected: BTreeSet<String> = ["src/index.ts"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected);
}

#[test]
fn walker_slash_bang_routes_to_ignore() {
    // Pinned 2026-08: the leading-`/` strip ran AFTER the `!`-parity
    // split, so `/!x` reached the matcher as whole-pattern negation
    // (single-pattern walks matched nearly everything; unions errored).
    // §4.3 now runs before §4.2: `/!x` ≡ `!x`.
    let t = TmpTree::new("slash_bang");
    t.touch("a.txt");
    t.touch("b.txt");

    let got = collect_rel(
        Walk::from_patterns(["**/*.txt", "/!b.txt"], t.root()).unwrap(),
        t.root(),
    );
    let expected: BTreeSet<String> = ["a.txt"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, expected, "`/!b.txt` must act as ignore, not negation");

    let got = collect_rel(Walk::new("/!b.txt", t.root()).unwrap(), t.root());
    assert!(
        got.is_empty(),
        "ignore-only input must yield nothing: {got:?}"
    );
}

#[cfg(unix)]
#[test]
fn walker_seed_emits_broken_symlink_like_walk() {
    // Pinned 2026-08: a broken symlink was emitted by `links/*` (walk
    // path, §10) but silently dropped by the literal `links/broken`
    // (seed path, §9.4) — the seed now falls back to symlink_metadata.
    let t = TmpTree::new("broken_seed");
    t.mkdir("links");
    std::os::unix::fs::symlink("/nonexistent-target-xyz", t.root().join("links/broken.txt"))
        .unwrap();

    let via_walk = collect_rel(Walk::new("links/*.txt", t.root()).unwrap(), t.root());
    let via_seed = collect_rel(Walk::new("links/broken.txt", t.root()).unwrap(), t.root());
    let expected: BTreeSet<String> = ["links/broken.txt"].iter().map(|s| s.to_string()).collect();
    assert_eq!(via_walk, expected);
    assert_eq!(via_seed, expected, "seed must surface the broken symlink");

    // The seeded entry's file_type is symlink-preserving, like every
    // walk-produced entry (entry.rs: the canonical is_symlink check).
    let entry = Walk::new("links/broken.txt", t.root())
        .unwrap()
        .next()
        .expect("one entry")
        .expect("no error");
    assert!(entry.file_type().is_symlink());
}

#[cfg(unix)]
#[test]
fn walker_seed_surfaces_permission_error() {
    use std::os::unix::fs::PermissionsExt;
    // Pinned 2026-08: an EACCES on the seed stat was silently swallowed
    // where a root walk would have yielded a loud per-entry error.
    let t = TmpTree::new("eacces_seed");
    t.touch("locked/b.txt");
    let locked = t.root().join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    // Running as root defeats the permission wall — skip there.
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let results: Vec<_> = Walk::new("locked/b.txt", t.root()).unwrap().collect();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(results.as_slice(), [Err(WalkError::Io { .. })]),
        "seed EACCES must surface as an Io error: {results:?}"
    );
}

#[test]
fn walker_normalizes_dot_and_doubleslash_patterns() {
    // Pinned 2026-08: `./` and `//` spellings leaked verbatim into
    // emitted paths (and inflated depth); patterns now normalize to the
    // walked-path spelling (§4.3), and a bare `.` matches nothing.
    let t = TmpTree::new("normalize");
    t.touch("a/b/x.txt");

    for pat in ["./a/b/*.txt", "a//b/*.txt", "a/./b/*.txt", ".//a/b/*.txt"] {
        let got = collect_rel(Walk::new(pat, t.root()).unwrap(), t.root());
        let expected: BTreeSet<String> = ["a/b/x.txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            got, expected,
            "pattern {pat:?} must emit the clean spelling"
        );
    }

    let entry = Walk::new("./a/b/*.txt", t.root())
        .unwrap()
        .next()
        .expect("one entry")
        .expect("no error");
    assert_eq!(entry.depth(), 3, "normalized prefix must not inflate depth");

    let got = collect_rel(Walk::new(".", t.root()).unwrap(), t.root());
    assert!(got.is_empty(), "`.` must not emit the base itself: {got:?}");
}

#[test]
fn walker_seed_order_is_forward() {
    // Pinned 2026-08: seeds were pushed in prefix order onto a LIFO
    // stack without the final reverse, so brace seeds walked backwards
    // (spec §7.2 step 1; JS walks forward).
    let t = TmpTree::new("seed_order");
    t.touch("sa/1.txt");
    t.touch("sb/2.txt");
    t.touch("sc/3.txt");

    let got: Vec<String> = Walk::new("{sc,sa,sb}/*.txt", t.root())
        .unwrap()
        .map(|r| r.unwrap().into_path())
        .map(|p| {
            p.strip_prefix(t.root())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        got,
        vec!["sa/1.txt", "sb/2.txt", "sc/3.txt"],
        "seeds must drain in (sorted) prefix order"
    );
}

#[cfg(unix)]
#[test]
fn walker_seed_emits_looping_symlink_like_walk() {
    // Any stat error probes lstat: a self-loop symlink (ELOOP) exists
    // as an entry, so seed and walk agree on emitting it.
    let t = TmpTree::new("loop_seed");
    t.mkdir("links");
    std::os::unix::fs::symlink("loop.txt", t.root().join("links/loop.txt")).unwrap();

    let expected: BTreeSet<String> = ["links/loop.txt"].iter().map(|s| s.to_string()).collect();
    let via_walk = collect_rel(Walk::new("links/*.txt", t.root()).unwrap(), t.root());
    let via_seed = collect_rel(Walk::new("links/loop.txt", t.root()).unwrap(), t.root());
    assert_eq!(via_walk, expected);
    assert_eq!(via_seed, expected);
}

#[test]
fn walker_missing_base_yields_error() {
    // A bad base fails loudly (one inline Err), not as a silent empty walk.
    let results: Vec<_> = Walk::new("**/*.rs", "/nonexistent-globstar-base")
        .unwrap()
        .collect();
    assert!(matches!(results.as_slice(), [Err(WalkError::Io { .. })]));
}

#[test]
fn walker_slash_only_pattern_is_dropped() {
    // A pattern that normalizes to nothing selects nothing — it must not
    // poison the sibling patterns.
    let t = TmpTree::new("slash_only");
    t.touch("a.txt");
    let got = collect_rel(
        Walk::from_patterns(["*.txt", "//"], t.root()).unwrap(),
        t.root(),
    );
    assert_eq!(got, ["a.txt"].iter().map(|s| s.to_string()).collect());
}
