//! The golden corpus: every `(pattern, path, expected)` row through the
//! public API and through the segment engine's NFA alone
//! (`CompileOptions::force_nfa`). Single-pattern rows are
//! `fixtures/corpus/corpus*.txt` and `fixtures/ecosystem/{js,rust}/*.txt`
//! (a row may expect `err:Kind`), `match_dir` rows `corpus-dir.txt`,
//! multi-pattern rows `corpus-multi.txt` and `corpus-multi-dir.txt`.
//! Nothing is skipped. The lines `corpus=single engine=globstar pass=N
//! fail=N` are what `scripts/verify.mjs` reads from `cargo test --test
//! corpus -- --nocapture`.

use globstar::{CompileOptions, DirMatch, Glob, GlobError};
use std::fmt::Debug;
use std::path::PathBuf;

const FIXTURES_DIR: &str = "../../fixtures";

const SPEC_FILES: &[&str] = &[
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
];

/// The ecosystem corpus was recorded on POSIX, so Windows skips it.
fn single_files() -> Vec<String> {
    let mut files: Vec<String> = SPEC_FILES.iter().map(|f| f.to_string()).collect();
    if cfg!(windows) {
        return files;
    }
    for side in ["js", "rust"] {
        let mut names: Vec<String> = std::fs::read_dir(corpus_path(&format!("ecosystem/{side}")))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".txt"))
            .collect();
        names.sort();
        files.extend(names.into_iter().map(|n| format!("ecosystem/{side}/{n}")));
    }
    files
}

fn corpus_path(name: &str) -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest).join(FIXTURES_DIR).join(name)
}

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

/// A field as bytes: a tab is written `⇥` and a newline `⏎`.
fn decode(s: &str) -> Vec<u8> {
    s.replace('⇥', "\t").replace('⏎', "\n").into_bytes()
}

/// `dot=` and `case_insensitive=`, each `true` or `false`; anything else
/// stops the run.
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

struct Cols {
    at: String,
    pattern: String,
    path: Vec<u8>,
    expected: String,
    dot: bool,
    case_insensitive: bool,
}

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
            path: decode(cols[1]),
            expected: cols[2].to_string(),
            dot,
            case_insensitive,
            at,
        });
    }
    rows
}

impl Cols {
    fn pattern(&self) -> String {
        String::from_utf8(decode(&self.pattern))
            .unwrap_or_else(|_| panic!("{}: pattern is not UTF-8", self.at))
    }

    fn patterns(&self) -> Vec<String> {
        match parse_json_string_array(&self.pattern) {
            Some(v) if !v.is_empty() => v,
            _ => panic!("{}: bad PATTERNS_JSON `{}`", self.at, self.pattern),
        }
    }

    /// `err:Kind`, or an answer `value` reads.
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

/// The patterns column of a multi-pattern row: a JSON array of strings,
/// read without `serde_json`.
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

#[test]
fn corpus_single_engines_vs_truth() {
    let mut globstar = Stats::default();
    let mut nfa = Stats::default();
    let mut rows = 0;
    for file in single_files() {
        for row in rows_of(&file, true) {
            rows += 1;
            let pattern = row.pattern();
            let want = row.want(parse_match);
            let g = Glob::new_with(&pattern, row.opts());
            globstar.record(judge(g.map(|g| g.is_match(&row.path)), &want), || {
                row.describe(&pattern, "globstar")
            });
            let g = Glob::new_with(&pattern, row.opts().force_nfa(true));
            nfa.record(judge(g.map(|g| g.is_match(&row.path)), &want), || {
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
        globstar.record(judge(g.map(|g| g.is_match(&row.path)), &want), || {
            row.describe(&patterns, "globstar")
        });
        let g = Glob::union_with(&patterns, row.opts().force_nfa(true));
        nfa.record(judge(g.map(|g| g.is_match(&row.path)), &want), || {
            row.describe(&patterns, "nfa")
        });
    }
    finish("multi", &[("globstar", &globstar), ("nfa", &nfa)]);
}

/// `corpus-dir.txt` rows default to `dot=false`, the walker convention.
/// Negated rows run too (GLOB_SPEC §15.4).
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
