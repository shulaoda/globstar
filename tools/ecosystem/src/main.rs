//! Ecosystem probe (Rust side). `scripts/ecosystem/generate.mjs` feeds it
//! the cases harvested from other libraries' test suites; it answers each
//! with every Rust matcher the benchmarks compare against, plus `globstar`
//! itself, so the corpus records what each library does at the version
//! `Cargo.lock` pins.
//!
//! ## Wire format
//!
//! One request per line, tab-separated: `<dot><ci>` (each `0`/`1`), the
//! pattern, the path. The fields use the corpus escapes (`\\`, `\t`, `\n`).
//! One answer line per request, tab-separated, in this order:
//!
//! `globstar`, `globset`, `fast-glob@1`, `fast-glob@0.4`, `wax`
//!
//! Each token is `match`, `no-match`, `err:<Kind>` (globstar), `error:<text>`
//! (another library refused the pattern) or `panic`. The probe does not
//! decide which options a library can express: it runs globset with
//! `case_insensitive` as asked and every library on the path as given; the
//! generator masks the rows a library cannot be asked about.
//!
//! globset runs with `literal_separator(true)` and `backslash_escape(true)`,
//! the configuration closest to this dialect (and the one GLOB_SPEC
//! Appendix A compares against).

use std::io::{self, BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use globstar::{CompileOptions, Glob};
use wax::Pattern as _;

fn main() {
    // A panicking library is an answer (`panic`), not the end of the run.
    panic::set_hook(Box::new(|_| {}));
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() != 3 || cols[0].len() != 2 {
            writeln!(out, "bad-request").expect("stdout write");
            continue;
        }
        let dot = cols[0].as_bytes()[0] == b'1';
        let ci = cols[0].as_bytes()[1] == b'1';
        let pattern = unescape(cols[1]);
        let path = unescape(cols[2]);
        let answers = [
            guarded(|| ours(&pattern, &path, dot, ci)),
            guarded(|| globset_answer(&pattern, &path, ci)),
            guarded(|| verdict(fast_glob_1::glob_match(&pattern, &path))),
            guarded(|| verdict(fast_glob::glob_match(&pattern, &path))),
            guarded(|| wax_answer(&pattern, &path)),
        ];
        writeln!(out, "{}", answers.join("\t")).expect("stdout write");
    }
    out.flush().expect("stdout flush");
}

fn guarded(f: impl FnOnce() -> String) -> String {
    panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| "panic".to_string())
}

fn verdict(matched: bool) -> String {
    if matched { "match" } else { "no-match" }.to_string()
}

fn ours(pattern: &str, path: &str, dot: bool, ci: bool) -> String {
    let opts = CompileOptions::default().dot(dot).case_insensitive(ci);
    match Glob::new_with(pattern, opts) {
        Ok(g) => verdict(g.is_match(path.as_bytes())),
        Err(e) => {
            // The variant name, as the corpus spells it: `err:Kind`.
            let debug = format!("{e:?}");
            let kind = debug.split([' ', '(', '{']).next().unwrap_or("Unknown");
            format!("err:{kind}")
        }
    }
}

fn globset_answer(pattern: &str, path: &str, ci: bool) -> String {
    let built = globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .case_insensitive(ci)
        .build();
    match built {
        Ok(g) => verdict(g.compile_matcher().is_match(Path::new(path))),
        Err(e) => error(&e.kind().to_string()),
    }
}

fn wax_answer(pattern: &str, path: &str) -> String {
    match wax::Glob::new(pattern) {
        Ok(g) => verdict(g.is_match(Path::new(path))),
        Err(e) => error(&e.to_string()),
    }
}

/// `error:<text>` on one line, short enough for a corpus comment.
fn error(text: &str) -> String {
    let flat: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();
    format!("error:{flat}")
}

/// The corpus escapes: `\\`, `\t`, `\n`; any other byte is itself.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
