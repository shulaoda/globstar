//! Integration test driver for the golden test corpus.
//!
//! Runs every `(pattern, path, expected)` row through the public engine
//! and asserts it agrees with the recorded truth:
//!
//! - `globstar`     — public API (`Glob::new` / `Glob::union`), engine dispatch
//! - `nfa`          — the same `Glob`, `is_match` forced onto the segment
//!   engine's NFA instead of its anchored chains (`Glob::is_match_nfa`)
//!
//! Single-pattern `is_match` rows live in `fixtures/corpus/corpus*.txt`
//! (a row may expect `err:Kind` instead of an answer); `match_dir` rows
//! in `fixtures/corpus/corpus-dir.txt`; multi-pattern rows in
//! `fixtures/corpus/corpus-multi.txt` and `corpus-multi-dir.txt`.
//!
//! Nothing is skipped: a compile error where a row expects an answer is
//! a failure, and a row the loader cannot read stops the run.
//!
//! Output lines like `corpus=single engine=globstar pass=N fail=N` are
//! picked up by the `scripts/verify.mjs` orchestrator when this file is
//! invoked via `cargo test --test corpus -- --nocapture`.

use globstar::{CompileOptions, DirMatch, Glob, GlobError};
use std::fmt::Debug;
use std::path::PathBuf;

const FIXTURES_DIR: &str = "../../fixtures";

/// The spec corpus, and the ecosystem corpus (`fixtures/ecosystem/`): the
/// cases other libraries' suites assert, with our answers. The suites were
/// run on POSIX, where the libraries themselves behave as recorded, so the
/// ecosystem rows run everywhere but Windows.
const SINGLE_FILES: &[&str] = &[
    "corpus/corpus.txt",
    "corpus/corpus-realworld.txt",
    "corpus/corpus-utf8.txt",
    "corpus/corpus-absolute.txt",
    "corpus/corpus-case.txt",
    "corpus/corpus-class.txt",
    "corpus/corpus-comprehensive.txt",
    "corpus/corpus-err.txt",
    #[cfg(windows)]
    "corpus/corpus-windows.txt",
    #[cfg(not(windows))]
    "corpus/corpus-unix.txt",
    #[cfg(not(windows))]
    "ecosystem/js/picomatch.txt",
    #[cfg(not(windows))]
    "ecosystem/js/picomatch-diff.txt",
    #[cfg(not(windows))]
    "ecosystem/js/micromatch.txt",
    #[cfg(not(windows))]
    "ecosystem/js/micromatch-diff.txt",
    #[cfg(not(windows))]
    "ecosystem/js/minimatch.txt",
    #[cfg(not(windows))]
    "ecosystem/js/minimatch-diff.txt",
    #[cfg(not(windows))]
    "ecosystem/rust/globset.txt",
    #[cfg(not(windows))]
    "ecosystem/rust/globset-diff.txt",
    #[cfg(not(windows))]
    "ecosystem/rust/fast-glob.txt",
    #[cfg(not(windows))]
    "ecosystem/rust/fast-glob-diff.txt",
    #[cfg(not(windows))]
    "ecosystem/rust/wax.txt",
    #[cfg(not(windows))]
    "ecosystem/rust/wax-diff.txt",
];

fn corpus_path(name: &str) -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest).join(FIXTURES_DIR).join(name)
}

/// What a row expects: an answer, or a compile error of this kind.
#[derive(Debug)]
enum Want<T> {
    Value(T),
    Err(String),
}

/// The `GlobError` variant's name, as the corpus files spell it.
fn error_kind(e: &GlobError) -> String {
    let debug = format!("{e:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or(&debug)
        .to_string()
}

/// `Ok` when `got` is what `want` asks for, else what went wrong.
fn judge<T: PartialEq + Debug>(got: Result<T, GlobError>, want: &Want<T>) -> Result<(), String> {
    match (got, want) {
        (Ok(v), Want::Value(w)) if v == *w => Ok(()),
        (Err(e), Want::Err(k)) if error_kind(&e) == *k => Ok(()),
        (Ok(v), _) => Err(format!("got {v:?}, expected {want:?}")),
        (Err(e), _) => Err(format!(
            "got error {} ({e}), expected {want:?}",
            error_kind(&e)
        )),
    }
}

#[derive(Default)]
struct Stats {
    pass: u32,
    fail: u32,
    failures: Vec<String>,
}

impl Stats {
    fn record(&mut self, verdict: Result<(), String>, row: impl FnOnce() -> String) {
        match verdict {
            Ok(()) => self.pass += 1,
            Err(why) => {
                self.fail += 1;
                if self.failures.len() < 10 {
                    self.failures.push(format!("{}: {why}", row()));
                }
            }
        }
    }

    fn print(&self, corpus: &str, engine: &str) {
        println!(
            "corpus={corpus:<6} engine={engine:<11} pass={} fail={}",
            self.pass, self.fail
        );
    }
}

/// Print every engine's line, then fail with their samples.
fn finish(corpus: &str, stats: &[(&str, &Stats)]) {
    for (engine, s) in stats {
        s.print(corpus, engine);
    }
    let total: u32 = stats.iter().map(|(_, s)| s.fail).sum();
    if total > 0 {
        for (_, s) in stats {
            for f in &s.failures {
                eprintln!("  FAIL: {f}");
            }
        }
        panic!("{total} {corpus} corpus assertion(s) failed");
    }
}

fn unescape(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\\' => out.push(b'\\'),
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                c => {
                    out.push(b'\\');
                    out.push(c);
                }
            }
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// The flags column: `dot=` and `case_insensitive=`, each `true` or
/// `false`; anything else is a typo and stops the run.
fn parse_flags(s: &str, default_dot: bool, at: &str) -> (bool, bool) {
    let mut dot = default_dot;
    let mut ci = false;
    for kv in s.split(',') {
        let (k, v) = kv
            .split_once('=')
            .unwrap_or_else(|| panic!("{at}: bad flag `{kv}`"));
        let v = match v.trim() {
            "true" => true,
            "false" => false,
            other => panic!("{at}: bad flag value `{other}`"),
        };
        match k.trim() {
            "dot" => dot = v,
            "case_insensitive" => ci = v,
            other => panic!("{at}: unknown flag `{other}`"),
        }
    }
    (dot, ci)
}

/// A row's columns: pattern, path, expected, and the flags (with the
/// default `dot`).
struct Cols {
    at: String,
    pattern: String,
    path: Vec<u8>,
    expected: String,
    dot: bool,
    case_insensitive: bool,
}

/// The data rows of `file`; a short row stops the run.
fn rows_of(file: &str, default_dot: bool) -> Vec<Cols> {
    let path = corpus_path(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {}", path.display(), e));
    let mut rows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = format!("{file}:{}", idx + 1);
        let cols: Vec<&str> = line.split('\t').collect();
        assert!(
            cols.len() >= 3,
            "{at}: a row needs PATTERN, PATH and EXPECTED"
        );
        let (dot, case_insensitive) = match cols.get(3) {
            Some(flags) => parse_flags(flags, default_dot, &at),
            None => (default_dot, false),
        };
        rows.push(Cols {
            pattern: cols[0].to_string(),
            path: unescape(cols[1]),
            expected: cols[2].to_string(),
            dot,
            case_insensitive,
            at,
        });
    }
    rows
}

impl Cols {
    /// The pattern column as text.
    fn pattern(&self) -> String {
        String::from_utf8(unescape(&self.pattern))
            .unwrap_or_else(|_| panic!("{}: pattern is not UTF-8", self.at))
    }

    /// The patterns column of a multi-pattern file.
    fn patterns(&self) -> Vec<String> {
        match parse_json_string_array(&self.pattern) {
            Some(v) if !v.is_empty() => v,
            _ => panic!("{}: bad PATTERNS_JSON `{}`", self.at, self.pattern),
        }
    }

    /// The expected column: `err:Kind`, or an answer `value` reads.
    fn want<T>(&self, value: impl Fn(&str) -> Option<T>) -> Want<T> {
        match self.expected.strip_prefix("err:") {
            Some(kind) => Want::Err(kind.to_string()),
            None => Want::Value(
                value(&self.expected)
                    .unwrap_or_else(|| panic!("{}: unknown expected `{}`", self.at, self.expected)),
            ),
        }
    }

    fn opts(&self) -> CompileOptions {
        CompileOptions::default()
            .dot(self.dot)
            .case_insensitive(self.case_insensitive)
    }

    /// The row as a failure message names it.
    fn describe(&self, pattern: &dyn Debug, engine: &str) -> String {
        format!(
            "{}: pattern={pattern:?} path={:?} dot={} ci={}: {engine}",
            self.at,
            String::from_utf8_lossy(&self.path),
            self.dot,
            self.case_insensitive,
        )
    }
}

fn parse_match(s: &str) -> Option<bool> {
    match s {
        "match" => Some(true),
        "no-match" => Some(false),
        _ => None,
    }
}

fn parse_dir(s: &str) -> Option<DirMatch> {
    match s {
        "pruned" => Some(DirMatch::Pruned),
        "descend" => Some(DirMatch::Descend),
        "match" => Some(DirMatch::Match),
        "descend-match" => Some(DirMatch::DescendAndMatch),
        "descend-all" => Some(DirMatch::DescendAll),
        "descend-all-match" => Some(DirMatch::DescendAllAndMatch),
        _ => None,
    }
}

/// Minimal JSON-array decoder for `corpus-multi.txt`'s patterns column —
/// patterns are plain strings with no embedded quotes, so a tiny
/// hand-rolled scanner avoids pulling in `serde_json`.
fn parse_json_string_array(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') || bytes.last() != Some(&b']') {
        return None;
    }
    let body = &s[1..s.len() - 1];
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_str = false;
    let mut escape = false;
    for ch in body.chars() {
        if escape {
            // Mirror the JSON-string escape set we actually care about.
            match ch {
                '"' | '\\' | '/' => buf.push(ch),
                'n' => buf.push('\n'),
                't' => buf.push('\t'),
                'r' => buf.push('\r'),
                _ => return None,
            }
            escape = false;
            continue;
        }
        if in_str {
            match ch {
                '\\' => escape = true,
                '"' => {
                    out.push(std::mem::take(&mut buf));
                    in_str = false;
                }
                _ => buf.push(ch),
            }
        } else {
            match ch {
                '"' => in_str = true,
                ',' | ' ' | '\t' => {}
                _ => return None,
            }
        }
    }
    if in_str {
        return None;
    }
    Some(out)
}

// ── tests ─────────────────────────────────────────────────────────────

#[test]
fn corpus_single_engines_vs_truth() {
    let mut globstar = Stats::default();
    let mut nfa = Stats::default();
    let mut rows = 0;
    for file in SINGLE_FILES {
        for row in rows_of(file, true) {
            rows += 1;
            let pattern = row.pattern();
            let want = row.want(parse_match);
            let g = Glob::new_with(&pattern, row.opts());
            globstar.record(
                judge(
                    g.as_ref()
                        .map(|g| g.is_match(&row.path))
                        .map_err(Clone::clone),
                    &want,
                ),
                || row.describe(&pattern, "globstar"),
            );
            nfa.record(judge(g.map(|g| g.is_match_nfa(&row.path)), &want), || {
                row.describe(&pattern, "nfa")
            });
        }
    }
    assert!(rows > 0, "no single-pattern corpus rows loaded");
    finish("single", &[("globstar", &globstar), ("nfa", &nfa)]);
}

#[test]
fn corpus_multi_engines_vs_truth() {
    let mut globstar = Stats::default();
    let mut nfa = Stats::default();
    let rows = rows_of("corpus/corpus-multi.txt", true);
    assert!(!rows.is_empty(), "no multi-pattern corpus rows loaded");
    for row in &rows {
        let patterns = row.patterns();
        let want = row.want(parse_match);
        let g = Glob::union_with(&patterns, row.opts());
        globstar.record(
            judge(
                g.as_ref()
                    .map(|g| g.is_match(&row.path))
                    .map_err(Clone::clone),
                &want,
            ),
            || row.describe(&patterns, "globstar"),
        );
        nfa.record(judge(g.map(|g| g.is_match_nfa(&row.path)), &want), || {
            row.describe(&patterns, "nfa")
        });
    }
    finish("multi", &[("globstar", &globstar), ("nfa", &nfa)]);
}

/// `corpus-dir.txt` rows default to `dot=false`, the walker convention;
/// rows for the other half of the matrix say `dot=true`. Negated rows
/// run too: `match_dir` answers `Descend`, or `DescendAndMatch` where the
/// directory itself matches, for them (GLOB_SPEC §15.4).
#[test]
fn corpus_dir_engines_vs_truth() {
    let mut globstar = Stats::default();
    let rows = rows_of("corpus/corpus-dir.txt", false);
    assert!(!rows.is_empty(), "no dir corpus rows loaded");
    for row in &rows {
        let pattern = row.pattern();
        let got = Glob::new_with(&pattern, row.opts()).map(|g| g.match_dir(&row.path));
        globstar.record(judge(got, &row.want(parse_dir)), || {
            row.describe(&pattern, "globstar")
        });
    }
    finish("dir", &[("globstar", &globstar)]);
}

#[test]
fn corpus_multi_dir_engines_vs_truth() {
    let mut globstar = Stats::default();
    let rows = rows_of("corpus/corpus-multi-dir.txt", false);
    assert!(!rows.is_empty(), "no multi-dir corpus rows loaded");
    for row in &rows {
        let patterns = row.patterns();
        let got = Glob::union_with(&patterns, row.opts()).map(|g| g.match_dir(&row.path));
        globstar.record(judge(got, &row.want(parse_dir)), || {
            row.describe(&patterns, "globstar")
        });
    }
    finish("mdir", &[("globstar", &globstar)]);
}
