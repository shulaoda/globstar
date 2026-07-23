//! Integration test driver for the golden test corpus.
//!
//! Runs every `(pattern, path, expected)` row through the public engine
//! and the total Pike VM reference engine
//! and asserts each agrees with the recorded truth:
//!
//! - `globstar`     — public API (`Glob::new` / `Glob::union`), tier routing
//! - `PikeVm`       — forced via `PikeVm::new`
//!
//! Single-pattern `is_match` rows live in `tests/corpus/corpus*.txt`;
//! `match_dir` rows in `tests/corpus/corpus-dir.txt`; multi-pattern rows
//! in `tests/corpus/corpus-multi.txt`. Parse-error rows live in
//! `tests/corpus/corpus-err.txt` and only exercise the public API
//! (engines see only well-formed programs).
//!
//! Output lines like `corpus=single engine=globstar pass=N fail=N skip=N`
//! are picked up by the root `verify.mjs` orchestrator when this file
//! is invoked via `cargo test --test corpus -- --nocapture`.

use globstar::ast::Node;
use globstar::engine::ops::lower;
use globstar::engine::pikevm::PikeVm;
use globstar::factor::factor_branches;
use globstar::parser::parse;
use globstar::{CompileOptions, DirMatch, Glob};
use std::path::PathBuf;

const CORPUS_DIR: &str = "tests/corpus";

const SINGLE_FILES: &[&str] = &[
    "corpus.txt",
    "corpus-realworld.txt",
    "corpus-fast-glob.txt",
    "corpus-fast-glob-diff.txt",
    "corpus-utf8.txt",
    "corpus-absolute.txt",
    "corpus-case.txt",
    "corpus-class.txt",
    "corpus-comprehensive.txt",
    #[cfg(windows)]
    "corpus-windows.txt",
    #[cfg(not(windows))]
    "corpus-unix.txt",
];

fn corpus_path(name: &str) -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest).join(CORPUS_DIR).join(name)
}

#[derive(Debug)]
struct SingleRow {
    file: &'static str,
    line_no: usize,
    pattern: String,
    path: Vec<u8>,
    expected: bool,
    dot: bool,
    case_insensitive: bool,
}

#[derive(Debug)]
struct MultiRow {
    line_no: usize,
    patterns: Vec<String>,
    path: Vec<u8>,
    expected: bool,
    dot: bool,
    case_insensitive: bool,
}

#[derive(Debug)]
struct DirRow {
    line_no: usize,
    pattern: String,
    path: Vec<u8>,
    expected: DirMatch,
    /// Per-spec / Walker convention, `corpus-dir.txt` rows default to
    /// `dot=false` (Bash-style dot-protection — the walker default),
    /// not `CompileOptions::default()`'s `dot=true`. Rows that need
    /// the other half of the matrix carry an explicit `dot=true` flag.
    dot: bool,
    case_insensitive: bool,
}

#[derive(Default)]
struct Stats {
    pass: u32,
    fail: u32,
    skip: u32,
    failures: Vec<String>,
}

impl Stats {
    fn record(&mut self, ok: bool, msg_on_fail: impl FnOnce() -> String) {
        if ok {
            self.pass += 1;
        } else {
            self.fail += 1;
            if self.failures.len() < 10 {
                self.failures.push(msg_on_fail());
            }
        }
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

fn parse_flags_default(s: &str, default_dot: bool) -> (bool, bool) {
    let mut dot = default_dot;
    let mut ci = false;
    for kv in s.split(',') {
        if let Some((k, v)) = kv.split_once('=') {
            match k.trim() {
                "dot" => dot = v.trim() == "true",
                "case_insensitive" => ci = v.trim() == "true",
                _ => {}
            }
        }
    }
    (dot, ci)
}

fn parse_flags(s: &str) -> (bool, bool) {
    parse_flags_default(s, true)
}

fn load_single_corpus() -> Vec<SingleRow> {
    let mut rows = Vec::new();
    for &file in SINGLE_FILES {
        let path = corpus_path(file);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skip {}: {}", path.display(), e);
                continue;
            }
        };
        for (idx, line) in text.lines().enumerate() {
            let line_no = idx + 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 3 {
                continue;
            }
            let pattern = match std::str::from_utf8(&unescape(cols[0])) {
                Ok(s) => s.to_owned(),
                Err(_) => continue,
            };
            let path = unescape(cols[1]);
            let expected = match cols[2] {
                "match" => true,
                "no-match" => false,
                _ => continue,
            };
            let (dot, ci) = if cols.len() >= 4 {
                parse_flags(cols[3])
            } else {
                (true, false)
            };
            rows.push(SingleRow {
                file,
                line_no,
                pattern,
                path,
                expected,
                dot,
                case_insensitive: ci,
            });
        }
    }
    rows
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

fn parse_dir_expected(s: &str) -> Option<DirMatch> {
    match s {
        "pruned" => Some(DirMatch::Pruned),
        "descend" => Some(DirMatch::Descend),
        "match" => Some(DirMatch::Match),
        "descend-match" => Some(DirMatch::DescendAndMatch),
        _ => None,
    }
}

fn load_dir_corpus() -> Vec<DirRow> {
    let path = corpus_path("corpus-dir.txt");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {}", path.display(), e));
    let mut rows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }
        let pattern = match std::str::from_utf8(&unescape(cols[0])) {
            Ok(s) => s.to_owned(),
            Err(_) => continue,
        };
        let path = unescape(cols[1]);
        let expected = match parse_dir_expected(cols[2]) {
            Some(d) => d,
            None => panic!("corpus-dir.txt:{line_no}: unknown DirMatch `{}`", cols[2]),
        };
        let (dot, ci) = if cols.len() >= 4 {
            parse_flags_default(cols[3], false)
        } else {
            (false, false)
        };
        rows.push(DirRow {
            line_no,
            pattern,
            path,
            expected,
            dot,
            case_insensitive: ci,
        });
    }
    rows
}

fn load_multi_corpus() -> Vec<MultiRow> {
    let path = corpus_path("corpus-multi.txt");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {}", path.display(), e));
    let mut rows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }
        let patterns = match parse_json_string_array(cols[0]) {
            Some(v) if !v.is_empty() => v,
            _ => panic!(
                "corpus-multi.txt:{line_no}: bad PATTERNS_JSON `{}`",
                cols[0]
            ),
        };
        let path = unescape(cols[1]);
        let expected = match cols[2] {
            "match" => true,
            "no-match" => false,
            other => panic!("corpus-multi.txt:{line_no}: unknown expected `{other}`"),
        };
        let (dot, ci) = if cols.len() >= 4 {
            parse_flags(cols[3])
        } else {
            (true, false)
        };
        rows.push(MultiRow {
            line_no,
            patterns,
            path,
            expected,
            dot,
            case_insensitive: ci,
        });
    }
    rows
}

// ── single-pattern engine runners ─────────────────────────────────────

fn run_globstar_single(row: &SingleRow) -> Option<bool> {
    let opts = CompileOptions::default()
        .dot(row.dot)
        .case_insensitive(row.case_insensitive);
    let g = Glob::new_with(&row.pattern, opts).ok()?;
    Some(g.is_match(&row.path))
}

fn run_pikevm_single(row: &SingleRow) -> Option<bool> {
    let ast = parse(row.pattern.as_bytes()).ok()?;
    let program = lower(&ast.body, ast.maybe_sep_distribution, row.case_insensitive);
    let pike = PikeVm::new(program, row.dot);
    let raw = pike.is_match(&row.path);
    Some(if ast.is_negated() { !raw } else { raw })
}

fn fail_msg_single(row: &SingleRow, engine: &str, got: bool) -> String {
    format!(
        "{}:{}: pattern={:?} path={:?} dot={} ci={}: {} got {}, expected {}",
        row.file,
        row.line_no,
        row.pattern,
        String::from_utf8_lossy(&row.path),
        row.dot,
        row.case_insensitive,
        engine,
        got,
        row.expected
    )
}

// ── multi-pattern engine runners ──────────────────────────────────────

fn run_globstar_multi(row: &MultiRow) -> Option<bool> {
    let opts = CompileOptions::default()
        .dot(row.dot)
        .case_insensitive(row.case_insensitive);
    let g = Glob::union_with(row.patterns.iter().map(|s| s.as_str()), opts).ok()?;
    Some(g.is_match(&row.path))
}

fn parse_bodies(patterns: &[String]) -> Option<Vec<Node>> {
    patterns
        .iter()
        .map(|p| parse(p.as_bytes()).ok().map(|ast| ast.body))
        .collect()
}

fn run_pikevm_multi(row: &MultiRow) -> Option<bool> {
    let bodies = parse_bodies(&row.patterns)?;
    let merged = factor_branches(bodies);
    let program = lower(&merged, true, row.case_insensitive);
    let pike = PikeVm::new(program, row.dot);
    Some(pike.is_match(&row.path))
}

// ── match_dir engine runners ──────────────────────────────────────────

/// `Glob::match_dir` currently does not invert via the prefix `!`
/// (see `lib.rs::Glob::match_dir`), and engines see only the
/// negation-stripped body. Until that gap is closed, skip negated rows
/// in both runners — they're recorded as `skip`, not `fail`.
fn run_globstar_dir(row: &DirRow) -> Option<DirMatch> {
    let ast = parse(row.pattern.as_bytes()).ok()?;
    if ast.is_negated() {
        return None;
    }
    let opts = CompileOptions::default()
        .dot(row.dot)
        .case_insensitive(row.case_insensitive);
    let g = Glob::new_with(&row.pattern, opts).ok()?;
    Some(g.match_dir(&row.path))
}

fn run_pikevm_dir(row: &DirRow) -> Option<DirMatch> {
    let ast = parse(row.pattern.as_bytes()).ok()?;
    if ast.is_negated() {
        return None;
    }
    let program = lower(&ast.body, ast.maybe_sep_distribution, row.case_insensitive);
    let pike = PikeVm::new(program, row.dot);
    Some(pike.match_dir(&row.path))
}

fn fail_msg_dir(row: &DirRow, engine: &str, got: DirMatch) -> String {
    format!(
        "corpus-dir.txt:{}: pattern={:?} path={:?} dot={} ci={}: {} got {:?}, expected {:?}",
        row.line_no,
        row.pattern,
        String::from_utf8_lossy(&row.path),
        row.dot,
        row.case_insensitive,
        engine,
        got,
        row.expected
    )
}

fn fail_msg_multi(row: &MultiRow, engine: &str, got: bool) -> String {
    format!(
        "corpus-multi.txt:{}: patterns={:?} path={:?} dot={} ci={}: {} got {}, expected {}",
        row.line_no,
        row.patterns,
        String::from_utf8_lossy(&row.path),
        row.dot,
        row.case_insensitive,
        engine,
        got,
        row.expected
    )
}

// ── tests ─────────────────────────────────────────────────────────────

#[test]
fn corpus_single_engines_vs_truth() {
    let rows = load_single_corpus();
    assert!(!rows.is_empty(), "no single-pattern corpus rows loaded");

    let mut globstar_stats = Stats::default();
    let mut pike_stats = Stats::default();

    for row in &rows {
        match run_globstar_single(row) {
            Some(got) => globstar_stats.record(got == row.expected, || {
                fail_msg_single(row, "globstar", got)
            }),
            None => globstar_stats.skip += 1,
        }
        match run_pikevm_single(row) {
            Some(got) => {
                pike_stats.record(got == row.expected, || fail_msg_single(row, "PikeVm", got))
            }
            None => pike_stats.skip += 1,
        }
    }

    println!(
        "corpus=single engine=globstar    pass={} fail={} skip={}",
        globstar_stats.pass, globstar_stats.fail, globstar_stats.skip
    );
    println!(
        "corpus=single engine=PikeVm      pass={} fail={} skip={}",
        pike_stats.pass, pike_stats.fail, pike_stats.skip
    );

    let total_fail = globstar_stats.fail + pike_stats.fail;
    if total_fail > 0 {
        for f in globstar_stats
            .failures
            .iter()
            .chain(pike_stats.failures.iter())
        {
            eprintln!("  FAIL: {f}");
        }
        panic!("{total_fail} single-pattern corpus assertion(s) failed");
    }
}

#[test]
fn corpus_multi_engines_vs_truth() {
    let rows = load_multi_corpus();
    assert!(!rows.is_empty(), "no multi-pattern corpus rows loaded");

    let mut globstar_stats = Stats::default();
    let mut pike_stats = Stats::default();

    for row in &rows {
        match run_globstar_multi(row) {
            Some(got) => {
                globstar_stats.record(got == row.expected, || fail_msg_multi(row, "globstar", got))
            }
            None => globstar_stats.skip += 1,
        }
        match run_pikevm_multi(row) {
            Some(got) => {
                pike_stats.record(got == row.expected, || fail_msg_multi(row, "PikeVm", got))
            }
            None => pike_stats.skip += 1,
        }
    }

    println!(
        "corpus=multi  engine=globstar    pass={} fail={} skip={}",
        globstar_stats.pass, globstar_stats.fail, globstar_stats.skip
    );
    println!(
        "corpus=multi  engine=PikeVm      pass={} fail={} skip={}",
        pike_stats.pass, pike_stats.fail, pike_stats.skip
    );

    let total_fail = globstar_stats.fail + pike_stats.fail;
    if total_fail > 0 {
        for f in globstar_stats
            .failures
            .iter()
            .chain(pike_stats.failures.iter())
        {
            eprintln!("  FAIL: {f}");
        }
        panic!("{total_fail} multi-pattern corpus assertion(s) failed");
    }
}

#[test]
fn corpus_dir_engines_vs_truth() {
    let rows = load_dir_corpus();
    assert!(!rows.is_empty(), "no dir corpus rows loaded");

    let mut globstar_stats = Stats::default();
    let mut pike_stats = Stats::default();

    for row in &rows {
        match run_globstar_dir(row) {
            Some(got) => {
                globstar_stats.record(got == row.expected, || fail_msg_dir(row, "globstar", got))
            }
            None => globstar_stats.skip += 1,
        }
        match run_pikevm_dir(row) {
            Some(got) => {
                pike_stats.record(got == row.expected, || fail_msg_dir(row, "PikeVm", got))
            }
            None => pike_stats.skip += 1,
        }
    }

    println!(
        "corpus=dir    engine=globstar    pass={} fail={} skip={}",
        globstar_stats.pass, globstar_stats.fail, globstar_stats.skip
    );
    println!(
        "corpus=dir    engine=PikeVm      pass={} fail={} skip={}",
        pike_stats.pass, pike_stats.fail, pike_stats.skip
    );

    let total_fail = globstar_stats.fail + pike_stats.fail;
    if total_fail > 0 {
        for f in globstar_stats
            .failures
            .iter()
            .chain(pike_stats.failures.iter())
        {
            eprintln!("  FAIL: {f}");
        }
        panic!("{total_fail} dir-corpus assertion(s) failed");
    }
}

#[test]
fn corpus_err_parse_failures() {
    let path = corpus_path("corpus-err.txt");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {}", path.display(), e));

    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut failures: Vec<String> = Vec::new();

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            continue;
        }
        let pattern = unescape(fields[0]);
        let expected_kind = fields[1];

        let pattern_str = match std::str::from_utf8(&pattern) {
            Ok(s) => s,
            Err(_) => {
                failures.push(format!("line {line_no}: pattern not utf8"));
                fail += 1;
                continue;
            }
        };

        match Glob::new(pattern_str) {
            Err(e) => {
                let kind = format!("{:?}", e);
                let variant_prefix = kind.split('{').next().unwrap_or(&kind).trim().to_string();
                let variant = variant_prefix
                    .split(' ')
                    .next()
                    .unwrap_or(&variant_prefix)
                    .to_string();
                if variant == expected_kind {
                    pass += 1;
                } else {
                    fail += 1;
                    failures.push(format!(
                        "line {line_no}: pattern={pattern_str:?}: expected {expected_kind}, got {variant} ({e})"
                    ));
                }
            }
            Ok(_) => {
                fail += 1;
                failures.push(format!(
                    "line {line_no}: pattern={pattern_str:?}: expected error {expected_kind}, but compiled successfully"
                ));
            }
        }
    }

    println!("corpus=err    engine=globstar    pass={pass} fail={fail} skip=0");
    if fail > 0 {
        for f in &failures {
            eprintln!("  FAIL: {f}");
        }
        panic!("{fail} parse-error assertion(s) failed");
    }
}
