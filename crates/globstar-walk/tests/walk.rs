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
    /// A fresh tree holding `files` (see [`touch`](Self::touch)).
    fn new(tag: &str, files: &[&str]) -> Self {
        let mut root = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        root.push(format!("glob-walker-test-{tag}-{pid}-{nanos}"));
        fs::create_dir_all(&root).expect("create tmp root");
        let tree = Self { root };
        for file in files {
            tree.touch(file);
        }
        tree
    }

    /// Create the file `rel` and its folders; a `rel` that ends in `/` is
    /// a folder.
    fn touch(&self, rel: &str) {
        let p = self.root.join(rel);
        if rel.ends_with('/') {
            fs::create_dir_all(&p).expect("create dir");
            return;
        }
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::File::create(&p).expect("create file");
    }

    fn root(&self) -> &Path {
        &self.root
    }

    /// What `patterns` yield under `opts`, relative to the root.
    fn walk_with(&self, patterns: &[&str], opts: WalkOptions) -> BTreeSet<String> {
        collect_rel(Walk::from_patterns(patterns, opts).unwrap(), &self.root)
    }

    /// What `patterns` yield from the root with the default options.
    fn walk(&self, patterns: &[&str]) -> BTreeSet<String> {
        self.walk_with(patterns, WalkOptions::new(&self.root))
    }
}

impl Drop for TmpTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// `items` as the set [`collect_rel`] gives.
fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
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

/// The pattern, reason and compile error of an `InvalidPattern`.
fn invalid(err: &WalkError) -> (&str, &str, Option<&globstar::GlobError>) {
    match err {
        WalkError::InvalidPattern {
            pattern,
            reason,
            source,
        } => (pattern, reason, source.as_ref()),
        _ => panic!("expected InvalidPattern, got {err:?}"),
    }
}

/// What patterns yield from one tree, compared as sets: seeds at literal
/// heads and at every brace branch, unions, `!` parity, folders that match,
/// and the options forwarded to the matcher.
#[test]
fn walker_answers() {
    let mut files = vec![
        "README.md",
        "package.json",
        "tsconfig.json",
        "main.js",
        "main.min.js",
        "src/main.rs",
        "src/lib.rs",
        "src/gen.rs",
        "src/notes.md",
        "src/main.ts",
        "src/cli/run.rs",
        "src/cli/commands/build.rs",
        "src/components/button.ts",
        "src/!(bundle.min).js",
        "src/app.js",
        "tests/smoke.rs",
        "tests/smoke.ts",
        "lib/vendored.rs",
        "node_modules/pkg/index.ts",
        "node_modules/pkg/package.json",
        "node_modules/deep/nested/b.ts",
        "dist/bundle.min.js",
        "crates/core/src/lib.rs",
        "crates/core/src/engine/mod.rs",
        "crates/core/tests/integration.rs",
        "crates/cli/src/main.rs",
        "a/b/c/d/e/f/g/h/leaf.rs",
        "a/b/c/d/shallow.rs",
        "a/shallow.rs",
        "target/debug/main",
        "dots/.env",
        "dots/main.rs",
        "case/Main.TS",
        "case/LIB.ts",
        "empty/",
    ];
    let flat: Vec<String> = (0..100).map(|i| format!("flat/item_{i}.txt")).collect();
    files.extend(flat.iter().map(String::as_str));
    files.push("flat/README.md");
    let t = TmpTree::new("answers", &files);
    let rows: &[(&[&str], &[&str])] = &[
        (&["src/main.rs"], &["src/main.rs"]),
        (&["src/nothere.rs"], &[]),
        (&["nothere/**/*.rs"], &[]),
        (&["empty/**/*.rs"], &[]),
        (&[], &[]),
        // A member with no literal head walks from the root.
        (
            &["package.json", "*.md", "src/*.rs"],
            &[
                "README.md",
                "package.json",
                "src/gen.rs",
                "src/lib.rs",
                "src/main.rs",
            ],
        ),
        (&["src/*.rs"], &["src/gen.rs", "src/lib.rs", "src/main.rs"]),
        (
            &["src/**/*.rs"],
            &[
                "src/cli/commands/build.rs",
                "src/cli/run.rs",
                "src/gen.rs",
                "src/lib.rs",
                "src/main.rs",
            ],
        ),
        (
            &["*.{json,md}"],
            &["README.md", "package.json", "tsconfig.json"],
        ),
        // Literals that share a suffix are each seeded whole.
        (
            &["{package,tsconfig}.json"],
            &["package.json", "tsconfig.json"],
        ),
        (
            &["package.json", "tsconfig.json"],
            &["package.json", "tsconfig.json"],
        ),
        (
            &["{src,tests}/*.rs"],
            &["src/gen.rs", "src/lib.rs", "src/main.rs", "tests/smoke.rs"],
        ),
        (
            &["src/*.rs", "tests/*.rs"],
            &["src/gen.rs", "src/lib.rs", "src/main.rs", "tests/smoke.rs"],
        ),
        (
            &["README.md", "src/*.ts", "tests/*.ts"],
            &["README.md", "src/main.ts", "tests/smoke.ts"],
        ),
        (
            &["src/**/*.ts", "node_modules/**/package.json"],
            &[
                "node_modules/pkg/package.json",
                "src/components/button.ts",
                "src/main.ts",
            ],
        ),
        (
            &["crates/*/src/**/*.rs", "crates/*/tests/**/*.rs"],
            &[
                "crates/cli/src/main.rs",
                "crates/core/src/engine/mod.rs",
                "crates/core/src/lib.rs",
                "crates/core/tests/integration.rs",
            ],
        ),
        (
            &["a/**/*.rs"],
            &[
                "a/b/c/d/e/f/g/h/leaf.rs",
                "a/b/c/d/shallow.rs",
                "a/shallow.rs",
            ],
        ),
        (&["**/*.min.js"], &["dist/bundle.min.js", "main.min.js"]),
        // `!` past the start is a literal byte: no extglob.
        (&["src/!(*.min).js"], &["src/!(bundle.min).js"]),
        (&["*.js", "!(*.min).js"], &["main.js", "main.min.js"]),
        // Leading `!`s route by parity: odd ignores, even selects.
        (&["!main.js"], &[]),
        (&["!!main.js"], &["main.js"]),
        (&["*.js", "!!!main.js"], &["main.min.js"]),
        // A folder that matches is yielded too.
        (&["src/components"], &["src/components"]),
        (&["target"], &["target"]),
        (&["dots/*"], &["dots/main.rs"]),
        (&["case/*.ts"], &["case/LIB.ts"]),
    ];
    for &(patterns, expected) in rows {
        assert_eq!(t.walk(patterns), set(expected), "{patterns:?}");
    }
    let flat: Vec<&str> = flat.iter().map(String::as_str).collect();
    assert_eq!(t.walk(&["flat/*.txt"]), set(&flat));

    let with = |patterns: &[&str], change: fn(&mut WalkOptions)| {
        let mut opts = WalkOptions::new(t.root());
        change(&mut opts);
        t.walk_with(patterns, opts)
    };
    assert_eq!(
        with(&["dots/*"], |o| o.dot = true),
        set(&["dots/.env", "dots/main.rs"])
    );
    assert_eq!(
        with(&["case/*.ts"], |o| o.case_insensitive = true),
        set(&["case/LIB.ts", "case/Main.TS"])
    );
    assert_eq!(
        with(&["src/*.rs"], |o| o.ignore = vec!["**/gen.rs".into()]),
        set(&["src/lib.rs", "src/main.rs"])
    );
    assert_eq!(
        with(&["**/*.ts"], |o| o.ignore =
            vec!["**/node_modules/**".into()]),
        set(&[
            "case/LIB.ts",
            "src/components/button.ts",
            "src/main.ts",
            "tests/smoke.ts"
        ])
    );
}

#[test]
fn walker_invalid_member_of_a_list_is_named() {
    let t = TmpTree::new("list_bad", &[]);
    // The bad member is named, not the whole list: a comma-joined list
    // could read as one valid pattern (`a/{b,c/**}`).
    let err = Walk::from_patterns(["src/**/*.{ts,tsx}", "lib/[abc"], t.root()).unwrap_err();
    let (pattern, reason, _) = invalid(&err);
    assert_eq!(pattern, "lib/[abc");
    assert!(
        reason.contains("unterminated character class at byte 4"),
        "{reason}"
    );
    let err = Walk::new(
        "*.rs",
        WalkOptions {
            ignore: vec!["a/{b".into(), "c/**".into()],
            ..WalkOptions::new(t.root())
        },
    )
    .unwrap_err();
    assert_eq!(invalid(&err).0, "a/{b");
    // Every member compiles alone; together they pass the expansion
    // budget (GLOB_SPEC §6.2). The error names the member it ran out in.
    let member = "{,/}".repeat(10) + "**/f";
    let members: Vec<String> = (0..10).map(|i| format!("{member}{i}")).collect();
    let err = Walk::from_patterns(&members, t.root()).unwrap_err();
    let (pattern, reason, _) = invalid(&err);
    assert_eq!(pattern, members[1]);
    assert!(reason.contains("the patterns share this limit"), "{reason}");
    // Found from the core's member index, not by compiling each member
    // again, so 5,000 members fail fast with a short message.
    let members: Vec<String> = (0..5000)
        .map(|i| format!("{}{{**,a}}/f{i}", "{**,a}{/,b}".repeat(4)))
        .collect();
    let start = std::time::Instant::now();
    let err = Walk::from_patterns(&members, t.root()).unwrap_err();
    assert!(start.elapsed().as_millis() < 500, "{:?}", start.elapsed());
    assert!(err.to_string().len() < 400, "{err}");
    // An error no one member explains (the union's compile work) names
    // the list, cut after three.
    let members = [
        "{a,/}{b,/}".repeat(300),
        "c{a,/}".repeat(300),
        "x".into(),
        "y".into(),
    ];
    let err = Walk::from_patterns(&members, t.root()).unwrap_err();
    let (pattern, _, source) = invalid(&err);
    assert!(pattern.ends_with("\"x\"] and 1 more"), "{pattern}");
    assert!(
        matches!(source, Some(globstar::GlobError::TooComplex { .. })),
        "{source:?}"
    );
}

/// Serializes tests that mutate process-wide CWD so they don't race
/// against each other. The rest of this file uses absolute paths from
/// `TmpTree::root()` and never touches CWD, so they don't need this guard.
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn walker_base_locked_to_construct_time_cwd() {
    // The default base `.` is made absolute when the walker is built: it
    // walks that folder, yields absolute paths, and a later `chdir` does
    // not move it.
    let _guard = CWD_LOCK.lock().expect("cwd lock poisoned");
    let t1 = TmpTree::new("locked_t1", &["only_in_t1.rs", "other.md"]);
    let t2 = TmpTree::new("locked_t2", &["only_in_t2.rs"]);
    let orig = std::env::current_dir().expect("save cwd");
    std::env::set_current_dir(t1.root()).expect("chdir into t1");
    let here = std::env::current_dir().expect("cwd");
    let walker = Walk::new("*.rs", WalkOptions::default()).expect("construct walker");
    std::env::set_current_dir(t2.root()).expect("chdir into t2");
    let paths: Vec<PathBuf> = walker
        .map(|r| r.expect("no walker error in test").into_path())
        .collect();
    std::env::set_current_dir(&orig).expect("restore cwd");
    assert_eq!(paths, [here.join("only_in_t1.rs")]);
}

#[test]
fn walker_leading_slash_stays_within_base() {
    // Security invariant: a pattern with a leading `/` must NOT let the
    // walker escape `opts.base`. Library callers that forward untrusted
    // user input as patterns rely on `opts.base` being a real sandbox;
    // silently propagating `Path::join`'s "absolute replaces base" would
    // be a path-traversal class of bug.
    let t_sandbox = TmpTree::new("leading_slash_sandbox", &["x.ts"]);
    let t_escape = TmpTree::new("leading_slash_escape_target", &["escape_me.ts"]);

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
    let t = TmpTree::new("ft_file_vs_dir", &["readme.md", "docs/intro.md"]);

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
    let t = TmpTree::new("depth", &["a.rs", "b/c.rs", "b/d/e.rs"]);

    let mut depths: Vec<usize> = Walk::new("**/*.rs", t.root())
        .unwrap()
        .map(|r| r.expect("no walker error").depth())
        .collect();
    depths.sort_unstable();
    assert_eq!(depths, vec![1, 2, 3]);
}

#[test]
fn dir_entry_file_name_returns_leaf() {
    let t = TmpTree::new("leaf_name", &["nested/deeply/target.txt"]);

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
    let t = TmpTree::new("meta_smoke", &[]);
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
fn walker_is_dir_follows_the_link() {
    // `is_dir` says what the walk took the entry for: a link to a folder
    // is a folder (and is descended), while `file_type` keeps the link.
    use std::os::unix::fs::symlink;
    let t = TmpTree::new("is_dir", &["real/a.txt"]);
    symlink(t.root().join("real"), t.root().join("link")).expect("create dir symlink");

    let mut dirs = BTreeSet::new();
    let mut files = BTreeSet::new();
    for entry in Walk::new("**", WalkOptions::new(t.root())).expect("compile") {
        let entry = entry.expect("no walker error in test");
        let rel = entry
            .path()
            .strip_prefix(t.root())
            .expect("strip root prefix")
            .to_string_lossy()
            .into_owned();
        if entry.is_dir() {
            assert!(
                entry.file_type().is_dir() || entry.file_type().is_symlink(),
                "{rel}"
            );
            dirs.insert(rel);
        } else {
            files.insert(rel);
        }
    }
    assert_eq!(dirs, set(&["link", "real"]));
    assert_eq!(files, set(&["link/a.txt", "real/a.txt"]));
}

#[cfg(target_os = "linux")]
#[test]
fn walker_skips_a_name_that_is_not_utf8() {
    // Linux keeps any bytes in a name; such a name is skipped, folder or
    // file, as the JS walker skips it (WALKER_SPEC §7.1).
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let t = TmpTree::new("nonutf8", &["src/ok.txt"]);
    let bad = t.root().join(OsStr::from_bytes(b"caf\xe9"));
    if fs::create_dir(&bad).is_err() {
        return; // the file system refuses such names
    }
    fs::File::create(bad.join("inner.txt")).expect("create file");
    fs::File::create(t.root().join(OsStr::from_bytes(b"caf\xe9.txt"))).expect("create file");

    for patterns in [&["**/*.txt"][..], &["*/*.txt"], &["*.txt", "*/*.txt"]] {
        assert_eq!(t.walk(patterns), set(&["src/ok.txt"]), "{patterns:?}");
    }
}

#[cfg(unix)]
#[test]
fn walker_follow_links_on_breaks_cycles() {
    // A directory symlink that loops back to its own ancestor would
    // cause infinite descent without cycle detection: a link back to
    // its ancestor is walked once more, then the cycle check stops it.
    use std::os::unix::fs::symlink;
    let t = TmpTree::new("follow_cycle", &["a.txt"]);
    // `t/loop` → `t` (the parent directory). Walking `t` would
    // visit `t/loop`, which IS `t`, which contains `loop` again …
    symlink(t.root(), t.root().join("loop")).expect("create dir-cycle symlink");

    let opts = WalkOptions {
        follow_links: true,
        ..WalkOptions::new(t.root())
    };
    let got = t.walk_with(&["**/*.txt"], opts);
    // Without cycle detection this would loop forever; with it the
    // descent into `loop` stops at the first re-entry, so the result is
    // finite and holds the real file.
    assert!(
        got.contains("a.txt"),
        "cycle protection must still yield the real file; got {got:?}"
    );
    assert!(
        got.iter().all(|p| !p.contains("loop/loop")),
        "cycle protection must cap depth at one re-entry; got {got:?}"
    );
}

// ── Seeds and pattern preprocessing ─────────────────────────────────────

/// A `..` in a pattern must not let the walk escape `base`, even
/// though the sibling directory exists on disk.
#[test]
fn walker_c2_dotdot_stays_in_base() {
    let outer = TmpTree::new("c2-outer", &["secret/passwd.txt", "sandbox/ok.txt"]);
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
    assert_eq!(got, set(&["ok.txt"]));
}

/// Seeding honors `follow_links = false`: a static prefix that lies on
/// or beyond a symlink is never reached by a root walk (which drops
/// symlinks), so a seed is not reached through it either.
#[test]
#[cfg(unix)]
fn walker_seed_symlink_follow_false() {
    use std::os::unix::fs::symlink;
    let t = TmpTree::new("seed-symlink", &["real/a.txt"]);
    symlink(t.root().join("real"), t.root().join("link")).unwrap();
    symlink(t.root().join("real/a.txt"), t.root().join("flink.txt")).unwrap();

    let opts = |follow: bool| WalkOptions {
        base: t.root().to_path_buf(),
        follow_links: follow,
        ..Default::default()
    };

    // follow = false: seeding through the symlinked prefix is suppressed.
    for pat in ["link/*.txt", "flink.txt"] {
        let got = t.walk_with(&[pat], opts(false));
        assert!(
            got.is_empty(),
            "follow=false seeded through symlink {pat:?}: {got:?}"
        );
    }
    // A root walk (empty prefix) reaches only the real file, not the links.
    let got = t.walk_with(&["**/*.txt"], opts(false));
    assert_eq!(got, set(&["real/a.txt"]));

    // follow = true: the seed is reached through the symlink.
    let got = t.walk_with(&["link/*.txt"], opts(true));
    assert_eq!(got, set(&["link/a.txt"]));
}

#[test]
fn walker_ignore_applies_to_seeded_prefixes() {
    // Seeds pass the ignore rules as walked entries do: a literal file,
    // a seeded folder and a seed under an ignored ancestor are all
    // dropped.
    let t = TmpTree::new("ignore_seed", &["src/generated/api.ts", "src/index.ts"]);

    let opts = |ignore: &[&str]| WalkOptions {
        ignore: ignore.iter().map(|s| s.to_string()).collect(),
        ..WalkOptions::new(t.root())
    };

    // Seeded literal FILE.
    let got = t.walk_with(&["src/generated/api.ts"], opts(&["**/generated/**"]));
    assert!(got.is_empty(), "seeded file escaped ignore: {got:?}");

    // §4.2 equivalence: the auto-split negative must behave identically.
    let got = t.walk(&["src/generated/api.ts", "!**/generated/**"]);
    assert!(
        got.is_empty(),
        "auto-split negative escaped seeding: {got:?}"
    );

    // Seeded DIR whose own relative path is ignored.
    let got = t.walk_with(&["src/generated/*.ts"], opts(&["**/generated"]));
    assert!(got.is_empty(), "seeded dir escaped ignore: {got:?}");

    // Seeded prefix under an ignored ANCESTOR directory.
    let got = t.walk_with(&["src/generated/api.ts"], opts(&["src"]));
    assert!(
        got.is_empty(),
        "seed under ignored ancestor escaped: {got:?}"
    );

    // Control: the non-seeded glob form agrees.
    let got = t.walk_with(&["src/**/*.ts"], opts(&["**/generated/**"]));
    assert_eq!(got, set(&["src/index.ts"]));
}

#[test]
fn walker_slash_bang_routes_to_ignore() {
    // Normalization (§4.3) runs before the `!` split (§4.2): `/!x` ≡
    // `!x`, an ignore entry, never whole-pattern negation.
    let t = TmpTree::new("slash_bang", &["a.txt", "b.txt"]);

    let got = t.walk(&["**/*.txt", "/!b.txt"]);
    assert_eq!(
        got,
        set(&["a.txt"]),
        "`/!b.txt` must act as ignore, not negation"
    );

    let got = t.walk(&["/!b.txt"]);
    assert!(
        got.is_empty(),
        "ignore-only input must yield nothing: {got:?}"
    );
}

#[cfg(unix)]
#[test]
fn walker_seed_emits_broken_symlink_like_walk() {
    // A literal seed yields a broken symlink as the walk does (§9.4,
    // §10): the seed is `lstat`-ed, so the link is an entry either way.
    let t = TmpTree::new("broken_seed", &["links/"]);
    std::os::unix::fs::symlink("/nonexistent-target-xyz", t.root().join("links/broken.txt"))
        .unwrap();

    let via_walk = t.walk(&["links/*.txt"]);
    let via_seed = t.walk(&["links/broken.txt"]);
    let expected = set(&["links/broken.txt"]);
    assert_eq!(via_walk, expected);
    assert_eq!(via_seed, expected, "seed must surface the broken symlink");

    // The seeded entry's file_type is symlink-preserving, like every
    // walk-produced entry (see DirEntry::file_type).
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
    // An EACCES on a seed's lstat is yielded as one Io error, as a root
    // walk yields it per entry.
    let t = TmpTree::new("eacces_seed", &["locked/b.txt"]);
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
    // `./`, `//` and `.` spellings normalize to the walked-path spelling
    // (§4.3): same path, same depth; a bare `.` matches nothing.
    let t = TmpTree::new("normalize", &["a/b/x.txt"]);

    for pat in ["./a/b/*.txt", "a//b/*.txt", "a/./b/*.txt", ".//a/b/*.txt"] {
        let got = t.walk(&[pat]);
        assert_eq!(
            got,
            set(&["a/b/x.txt"]),
            "{pat:?} must emit the clean spelling"
        );
    }

    let entry = Walk::new("./a/b/*.txt", t.root())
        .unwrap()
        .next()
        .expect("one entry")
        .expect("no error");
    assert_eq!(entry.depth(), 3, "normalized prefix must not inflate depth");

    let got = t.walk(&["."]);
    assert!(got.is_empty(), "`.` must not emit the base itself: {got:?}");
}

#[test]
fn walker_seed_order_is_forward() {
    // Seeds drain in sorted prefix order (WALKER_SPEC §7.2 step 1), as
    // in JS.
    let t = TmpTree::new("seed_order", &["sa/1.txt", "sb/2.txt", "sc/3.txt"]);

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
    // A seed part is lstat-ed, so a self-loop symlink (ELOOP) is an
    // entry: seed and walk agree on emitting it.
    let t = TmpTree::new("loop_seed", &["links/"]);
    std::os::unix::fs::symlink("loop.txt", t.root().join("links/loop.txt")).unwrap();

    let expected = set(&["links/loop.txt"]);
    let via_walk = t.walk(&["links/*.txt"]);
    let via_seed = t.walk(&["links/loop.txt"]);
    assert_eq!(via_walk, expected);
    assert_eq!(via_seed, expected);
}

#[test]
fn walker_bad_base_fails_for_every_pattern_shape() {
    // The base is checked whatever the pattern's shape: a missing or
    // non-folder base gives one Io error, never a silent empty walk.
    let t = TmpTree::new("bad_base", &["file.txt"]);
    let file = t.root().join("file.txt");
    for base in [Path::new("/nonexistent-globstar-base"), file.as_path()] {
        for pattern in ["src/*.rs", "Cargo.toml", "**/*.rs"] {
            let results: Vec<_> = Walk::new(pattern, base).unwrap().collect();
            assert!(
                matches!(results.as_slice(), [Err(WalkError::Io { .. })]),
                "{pattern} under {}: {results:?}",
                base.display()
            );
        }
    }
}

#[test]
fn walker_seed_with_an_empty_segment_is_dropped() {
    // `{a,/b}` gives the prefix `/b`: no walked path holds an empty
    // segment, so a root walk never reaches it.
    let t = TmpTree::new("empty_seg", &["a/x", "a/b/x", "b/x"]);
    assert_eq!(t.walk(&["{a,/b}/x"]), set(&["a/x"]));
    assert_eq!(t.walk(&["{/a,/b}/x"]), set(&[]));
    // A written `//` is collapsed first (WALKER_SPEC §4.3).
    assert_eq!(t.walk(&["{a//b,b}/x"]), set(&["a/b/x", "b/x"]));
}

#[cfg(unix)]
#[test]
fn walker_seed_errors_follow_the_root_walk() {
    // Nothing can be below a looping link, past a name too long for
    // the filesystem, or at a name with a NUL: a root walk lists none of
    // them, so a seed there is silent too.
    let t = TmpTree::new("seed_errors", &["ok.txt"]);
    std::os::unix::fs::symlink("loop", t.root().join("loop")).unwrap();
    assert_eq!(t.walk(&["{ok.txt,loop/x.txt}"]), set(&["ok.txt"]));
    assert_eq!(t.walk(&[&("a".repeat(300) + "/*.txt")]), set(&[]));
    assert_eq!(t.walk(&["{ok.txt,a\0b/x.txt}"]), set(&["ok.txt"]));
}

#[cfg(unix)]
#[test]
fn walker_seed_through_a_symlink_knows_its_cycle() {
    // A root walk reaches `a/link` holding the link's target, so it does
    // not go round `a/link/link`; a seed must start the same way.
    let t = TmpTree::new("seed_cycle", &["a/x.txt"]);
    std::os::unix::fs::symlink(".", t.root().join("a/link")).unwrap();
    let walk = |p: &str| t.walk(&[p]);
    for (seeded, root) in [
        ("a/link/**/*.txt", "*/link/**/*.txt"),
        ("a/link/link/**/*.txt", "*/link/link/**/*.txt"),
        ("a/link/link/x.txt", "*/link/link/x.txt"),
    ] {
        assert_eq!(walk(seeded), walk(root), "{seeded}");
    }
    assert_eq!(walk("a/link/**/*.txt"), set(&["a/link/x.txt"]));
}

#[test]
fn walker_trailing_dot_names_the_folder() {
    // `d/.` is the folder `d` (`d/`, WALKER_SPEC §4.3), so `*/.` names
    // folders only. As a pattern it yields the folders it names;
    // as an ignore entry it prunes them (§3.2).
    let t = TmpTree::new("trailing_dot", &["x.txt", "d/y.txt", "d/e/z.txt"]);
    assert_eq!(t.walk(&["*/."]), set(&["d"]));
    assert_eq!(t.walk(&["**/."]), set(&["d", "d/e"]));
    let ignoring = |entry: &str| {
        let opts = WalkOptions {
            ignore: vec![entry.to_string()],
            ..WalkOptions::new(t.root())
        };
        t.walk_with(&["**"], opts)
    };
    assert_eq!(ignoring("*/."), set(&["x.txt"]));
    assert_eq!(ignoring("d/e/."), set(&["x.txt", "d", "d/y.txt"]));
}

#[cfg(unix)]
#[test]
fn walker_base_resolves_dot_dot_as_text() {
    // As Node's `path.resolve`: `jump/..` is the folder holding `jump`,
    // not the parent of the link's target, and `..` never reaches the
    // output.
    let t = TmpTree::new(
        "base_dotdot",
        &["top.txt", "real/inner/in.txt", "real/r.txt"],
    );
    std::os::unix::fs::symlink("real/inner", t.root().join("jump")).unwrap();
    let base = t.root().join("jump/..");
    let got: Vec<PathBuf> = Walk::new("*.txt", base.as_path())
        .unwrap()
        .map(|r| r.unwrap().into_path())
        .collect();
    assert_eq!(got, [t.root().join("top.txt")]);
    let roundabout = t.root().join("nonexistent/../real");
    let got = collect_rel(
        Walk::new("*.txt", roundabout.as_path()).unwrap(),
        &t.root().join("real"),
    );
    assert_eq!(got, set(&["r.txt"]));
}

#[test]
fn walker_empty_base_is_the_current_folder() {
    // As `path.resolve("")` in JS.
    let _guard = CWD_LOCK.lock().expect("cwd lock poisoned");
    let t = TmpTree::new("empty_base", &["here.txt"]);
    let orig = std::env::current_dir().expect("save cwd");
    std::env::set_current_dir(t.root()).expect("chdir into tmp");
    let here = std::env::current_dir().expect("cwd");
    let walker = Walk::new("*.txt", "").unwrap();
    std::env::set_current_dir(&orig).expect("restore cwd");
    assert_eq!(walker.base(), here);
    let got: Vec<_> = walker.map(|r| r.unwrap().into_path()).collect();
    assert_eq!(got, [here.join("here.txt")]);
}

#[test]
fn walker_error_names_the_pattern_as_written() {
    let err = Walk::from_patterns(["src/**", "!src/[z-a]"], ".").unwrap_err();
    let (pattern, reason, _) = invalid(&err);
    assert_eq!(pattern, "!src/[z-a]");
    assert!(
        reason.contains("byte 5") && reason.contains("'src/[z-a]'"),
        "{reason}"
    );
    // A pattern that needs no normalization is quoted as is.
    let err = Walk::new("src/[z-a]", ".").unwrap_err();
    assert!(!err.to_string().contains("as the walker reads it"), "{err}");
}

#[test]
fn walker_seeds_see_the_tree_when_iterated() {
    // Seeds are laid out on the first `next()`, as folders are read:
    // a file gone by then is gone for both.
    let t = TmpTree::new("lazy_seed", &["a.txt"]);
    let literal = Walk::new("a.txt", t.root()).unwrap();
    let glob = Walk::new("*.txt", t.root()).unwrap();
    fs::remove_file(t.root().join("a.txt")).unwrap();
    assert_eq!(collect_rel(literal, t.root()), set(&[]));
    assert_eq!(collect_rel(glob, t.root()), set(&[]));
}

#[cfg(unix)]
#[test]
fn walker_skips_a_subtree_the_ignore_covers() {
    // Under dot=true `**/node_modules/**/*` covers all of `node_modules`,
    // so the walk never reads it: an unreadable `node_modules` raises
    // no error. (The entry does not match `node_modules/` itself, so
    // only the all-below check can prune it.)
    use std::os::unix::fs::PermissionsExt;
    let t = TmpTree::new("ignore_prune", &["src/a.js", "node_modules/p/f.js"]);
    let locked = t.root().join("node_modules");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&locked).is_ok(); // root reads anything
    let walk = |dot: bool| {
        let opts = WalkOptions {
            base: t.root().to_path_buf(),
            dot,
            ignore: vec!["**/node_modules/**/*".to_string()],
            ..WalkOptions::default()
        };
        Walk::new("**/*.js", opts).unwrap().collect::<Vec<_>>()
    };
    let pruned = walk(true);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return;
    }
    let paths: Vec<_> = pruned.into_iter().map(|r| r.unwrap().into_path()).collect();
    assert_eq!(paths, [t.root().join("src/a.js")]);
}

#[test]
fn walker_bangs_exposed_by_normalization_count_too() {
    // `!/!x` ≡ `!!x` ≡ `x`: a `/` between two `!` runs hides neither.
    let t = TmpTree::new("bang_fold", &["x", "y", "d/z"]);
    assert_eq!(t.walk(&["!/!x"]), set(&["x"]));
    assert_eq!(t.walk(&["**", "!!/!x"]), set(&["y", "d", "d/z"]));
    assert_eq!(t.walk(&["**", "!/!/!x"]), set(&["y", "d", "d/z"]));
    // A run that leaves `.` hides nothing either; `!.`, `!/` and
    // `!!/.` select nothing.
    assert_eq!(t.walk(&["!./!x"]), set(&["x"]));
    assert_eq!(t.walk(&["**", "!./!./!x"]), set(&["y", "d", "d/z"]));
    assert_eq!(t.walk(&["x", "!.", "!/", "!!/."]), set(&["x"]));
}

#[test]
fn walker_splits_bangs_in_one_pass() {
    // A 64 KB `!/!/…x` took seconds when each `!` run normalized the
    // rest of the pattern again. Twin of the check in walk.mjs.
    let t = TmpTree::new("bang_pass", &["x"]);
    for p in ["!/".repeat(32_000) + "x", "!./".repeat(21_000) + "x"] {
        let start = std::time::Instant::now();
        assert_eq!(t.walk(&[p.as_str()]), set(&["x"]));
        assert!(start.elapsed().as_millis() < 500, "{:?}", start.elapsed());
    }
}

#[test]
fn walker_refuses_an_ignore_entry_that_starts_with_bang() {
    // `!src/**` as an ignore entry would ignore everything else, so it is
    // refused, alone or in a list.
    for ignore in [vec!["!src/**"], vec!["!src/**", "node_modules"]] {
        let opts = WalkOptions {
            ignore: ignore.iter().map(|s| s.to_string()).collect(),
            ..WalkOptions::default()
        };
        let err = Walk::new("**", opts).unwrap_err();
        let (pattern, _, source) = invalid(&err);
        assert_eq!(pattern, "!src/**");
        assert!(source.is_none());
        assert!(err.to_string().contains("cannot start with `!`"), "{err}");
    }
}

#[test]
fn walker_takes_any_path_like_base() {
    // `&String`, `&OsStr` and `OsString` convert like `&str` and paths.
    let t = TmpTree::new("path_like", &["a.txt"]);
    let s = t.root().to_string_lossy().into_owned();
    let os = t.root().as_os_str();
    for walk in [
        Walk::new("*.txt", &s).unwrap(),
        Walk::new("*.txt", os).unwrap(),
        Walk::new("*.txt", os.to_os_string()).unwrap(),
    ] {
        assert_eq!(collect_rel(walk, t.root()).len(), 1);
    }
}

#[test]
fn walker_pattern_error_keeps_its_glob_error() {
    use std::error::Error;
    let err = Walk::new("[z-a]", ".").unwrap_err();
    assert_eq!(invalid(&err).0, "[z-a]");
    let source = err.source().expect("a source");
    assert!(matches!(
        source.downcast_ref::<globstar::GlobError>(),
        Some(globstar::GlobError::InvalidRange { .. })
    ));
}

#[test]
fn walker_slash_only_pattern_is_dropped() {
    // A pattern that normalizes to nothing selects nothing — it must not
    // poison the sibling patterns.
    let t = TmpTree::new("slash_only", &["a.txt"]);
    assert_eq!(t.walk(&["*.txt", "//"]), set(&["a.txt"]));
}

// Windows: a seed reaches only what a root walk lists, under the spelling
// it lists it (WALKER_SPEC §6).
#[cfg(windows)]
#[test]
fn windows_seeds_reach_what_a_root_walk_lists() {
    let t = TmpTree::new("win-seeds", &["src/a.ts", "src/sub/b.ts", "secret/key.pem"]);
    let base = t.root();
    let walk = |patterns: &[&str], ignore: &[&str]| -> Vec<String> {
        let mut opts = WalkOptions::new(base);
        opts.ignore = ignore.iter().map(|s| s.to_string()).collect();
        let mut out: Vec<String> = Walk::from_patterns(patterns.iter().copied(), opts)
            .unwrap()
            .map(|r| {
                r.expect("no walker error")
                    .into_path()
                    .display()
                    .to_string()
            })
            .collect();
        out.sort();
        out
    };
    // One spelling per file, native `\`, whatever the pattern's shape; an
    // escaped `\` is no separator, so its member adds no second copy.
    let b = base
        .join("src")
        .join("sub")
        .join("b.ts")
        .display()
        .to_string();
    for patterns in [
        &["**/b.ts"][..],
        &["src/sub/*.ts"],
        &["src/sub/b.ts"],
        &["src\\\\sub/*.ts", "src/sub/*.ts"],
    ] {
        assert_eq!(
            walk(patterns, &[]),
            std::slice::from_ref(&b),
            "{patterns:?}"
        );
    }
    // A drive is never below base, and a name Win32 rewrites (`src.`,
    // `src `, `nul`, `con.txt`) reaches no folder a root walk lists: no
    // match, no error, nothing past the ignore rules.
    let drive = format!("{}/src/*.ts", base.display().to_string().replace('\\', "/"));
    for patterns in [
        &[drive.as_str()][..],
        &["src./*.ts"],
        &["src /*.ts"],
        &["src/a.ts."],
        &["nul"],
        &["con.txt"],
        &["secret./*.pem"],
    ] {
        assert!(walk(patterns, &["secret"]).is_empty(), "{patterns:?}");
    }
}

// A seed finds a name only in the spelling its folder lists it: APFS
// opens an NFC spelling of a name stored NFD, which a root walk, comparing
// names as listed, never matches. Twin of the check in walk.mjs.
#[test]
fn a_seed_finds_a_name_only_as_listed() {
    let t = TmpTree::new("nfd", &[]);
    let nfd = "e\u{301}";
    // A file system that does not keep the name as given (wine's) has
    // nothing to test.
    let kept = std::panic::catch_unwind(|| {
        t.touch(&format!("{nfd}.txt"));
        t.touch(&format!("{nfd}/a.ts"));
    })
    .is_ok()
        && fs::read_dir(t.root())
            .unwrap()
            .flatten()
            .any(|e| e.file_name() == format!("{nfd}.txt").as_str());
    if !kept {
        return;
    }
    assert!(t.walk(&["\u{e9}.txt"]).is_empty());
    assert!(t.walk(&["\u{e9}/*.ts"]).is_empty());
    assert_eq!(t.walk(&[&format!("{nfd}.txt")]).len(), 1);
    assert_eq!(t.walk(&[&format!("{nfd}/*.ts")]).len(), 1);
}

// A folder removed, or made a file, after its parent listed it is left
// out, as if it had gone before: the rest of the walk goes on, with no
// error. Twin of the check in walk.mjs.
#[test]
fn a_folder_gone_before_its_read_is_left_out() {
    let t = TmpTree::new("gone", &["a/1.txt", "b/2.txt", "b/c/3.txt", "d/4.txt"]);
    let mut walk = Walk::new("**/*.txt", t.root()).unwrap();
    // The base holds folders only, so the first entry comes from the
    // first of them read, and the other two are listed but not read.
    let first = walk.next().unwrap().unwrap().into_path();
    let top = first.strip_prefix(t.root()).unwrap().iter().next().unwrap();
    let others: Vec<_> = ["a", "b", "d"].into_iter().filter(|&d| top != d).collect();
    fs::remove_dir_all(t.root().join(others[0])).unwrap();
    fs::remove_dir_all(t.root().join(others[1])).unwrap();
    fs::write(t.root().join(others[1]), "now a file").unwrap();
    for entry in walk {
        let path = entry.unwrap().into_path();
        assert!(path.starts_with(t.root().join(top)), "{path:?}");
    }
}

// `!` and `!!` negate nothing: an empty pattern (GLOB_SPEC §6.1), named as
// written. Twin of the check in walk.mjs.
#[test]
fn a_lone_bang_is_an_empty_pattern() {
    let t = TmpTree::new("lone-bang", &["a.js"]);
    for lone in ["!", "!!"] {
        let err = Walk::from_patterns(["**", lone], t.root()).unwrap_err();
        let (pattern, _, source) = invalid(&err);
        assert_eq!(pattern, lone);
        assert!(
            matches!(source, Some(globstar::GlobError::Empty)),
            "{source:?}"
        );
    }
}

// A FIFO, a socket or a device is no file to read (a reader would block
// on a FIFO). A link takes its target's type; a dangling one is still
// emitted. Twin of the check in walk.mjs.
#[cfg(unix)]
#[test]
fn fifos_and_sockets_are_no_files() {
    use std::os::unix::fs::symlink;
    let t = TmpTree::new("sp", &["a.txt"]);
    let root = t.root();
    let made = std::process::Command::new("mkfifo")
        .arg(root.join("fifo"))
        .status()
        .unwrap();
    assert!(made.success());
    let _sock = std::os::unix::net::UnixListener::bind(root.join("sock")).unwrap();
    symlink("fifo", root.join("to-fifo")).unwrap();
    symlink("a.txt", root.join("to-a")).unwrap();
    symlink("missing", root.join("dangling")).unwrap();
    assert_eq!(t.walk(&["*"]), set(&["a.txt", "dangling", "to-a"]));
    for seeded in ["fifo", "sock", "to-fifo"] {
        assert!(t.walk(&[seeded]).is_empty(), "{seeded}");
    }
}

// A pattern or ignore entry that ends in `/` matches folders only
// (WALKER_SPEC §3.2): `node_modules/` prunes the folder, never a file;
// `src/` yields the folder. Twin of the check in walk.mjs.
#[test]
fn a_pattern_that_ends_in_a_slash_matches_folders_only() {
    let t = TmpTree::new(
        "folders",
        &[
            "node_modules/p/i.js",
            "src/a.js",
            "src/node_modules/q.js",
            "build",
            "dist/x.js",
        ],
    );
    let walk = |patterns: &[&str], ignore: &[&str]| {
        let mut opts = WalkOptions::new(t.root());
        opts.ignore = ignore.iter().map(|s| s.to_string()).collect();
        t.walk_with(patterns, opts)
    };
    let cases: [(&[&str], &[&str], &[&str]); 9] = [
        (
            &["**/*.js"],
            &["node_modules/"],
            &["dist/x.js", "src/a.js", "src/node_modules/q.js"],
        ),
        (
            &["**/*.js"],
            &["**/node_modules/"],
            &["dist/x.js", "src/a.js"],
        ),
        (
            &["*"],
            &["build/"],
            &["build", "dist", "node_modules", "src"],
        ),
        (
            &["**/*.js", "!dist/"],
            &[],
            &["node_modules/p/i.js", "src/a.js", "src/node_modules/q.js"],
        ),
        (&["node_modules/p/*.js"], &["node_modules/"], &[]),
        (&["src/"], &[], &["src"]),
        (&["build/"], &[], &[]),
        (&["*/"], &[], &["dist", "node_modules", "src"]),
        (&["*/"], &["src/"], &["dist", "node_modules"]),
    ];
    for (patterns, ignore, want) in cases {
        assert_eq!(
            walk(patterns, ignore),
            set(want),
            "{patterns:?} ignore {ignore:?}"
        );
    }
}

// An ignore entry that ends in `/**` prunes the folder itself, under
// either `dot` (WALKER_SPEC §3.2): `node_modules` is never read, and a
// dot-led name below it that a pattern names is not found, as fast-glob
// and tinyglobby do not find it. Twin of the check in walk.mjs.
#[test]
fn an_ignore_that_ends_in_a_globstar_prunes_the_folder() {
    let t = TmpTree::new(
        "prunestar",
        &[
            "node_modules/p/i.js",
            "node_modules/.cache/x",
            "src/a.js",
            "src/.env",
        ],
    );
    for dot in [true, false] {
        let mut opts = WalkOptions::new(t.root());
        opts.dot = dot;
        opts.ignore = vec!["**/node_modules/**".to_string()];
        let patterns = ["**/*.js", "**/.env", "node_modules/.cache/x"];
        let got = t.walk_with(&patterns, opts);
        assert_eq!(got, set(&["src/.env", "src/a.js"]), "dot={dot}");
    }
}

// A seed is reached as a root walk reaches it: through the walk's own
// per-entry rule, so a folder the ignore set covers is pruned before it
// is read, from a seed as from the root. Twin of the check in walk.mjs.
#[cfg(unix)]
#[test]
fn a_seed_under_a_folder_the_ignore_set_covers_is_not_read() {
    use std::os::unix::fs::PermissionsExt;
    let t = TmpTree::new("covered-seed", &["node_modules/p/i.js", "a.js"]);
    let locked = t.root().join("node_modules");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let walk = |pattern: &str| {
        let opts = WalkOptions {
            base: t.root().to_path_buf(),
            dot: true,
            ignore: vec!["**/node_modules/**".into()],
            ..WalkOptions::default()
        };
        Walk::new(pattern, opts).unwrap().collect::<Vec<_>>()
    };
    let got: Vec<_> = [
        "node_modules/**/*.js",
        "[n]ode_modules/**/*.js",
        "{node_modules,zz}/**/*.js",
    ]
    .map(|p| (p, walk(p).len()))
    .into();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    for (pattern, items) in got {
        assert_eq!(items, 0, "{pattern}");
    }
}
