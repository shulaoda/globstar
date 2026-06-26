//! Cross-runtime differential-test harness (Rust side).
//!
//! Reads one request per line on stdin and writes one result token per line
//! on stdout, using ONLY `globstar`'s public API — exactly what a real caller
//! sees. The JS driver (`fuzz.mjs` at the repo root) generates random inputs,
//! feeds the SAME inputs to this binary and to the `@globstar/globstar` JS
//! port, and asserts the two agree. This measures JS↔Rust drift on the
//! infinite out-of-corpus input space, which the fixed hand corpus cannot.
//!
//! ## Wire format
//!
//! Tab-separated. The first field is the command, the second is two flag
//! chars `dot` then `case_insensitive` (each `0`/`1`, e.g. `10` = dot on,
//! ci off). Every following field is a byte-escaped token.
//!
//! | line                                  | op                | output tokens                                   |
//! |---------------------------------------|-------------------|-------------------------------------------------|
//! | `m <flags> <pattern> <path>`          | `Glob::is_match`  | `match` / `no-match` / `err:<Kind>`             |
//! | `d <flags> <pattern> <dir>`           | `Glob::match_dir` | `pruned` / `descend` / `match` / `descend-match` / `err:<Kind>` |
//! | `u <flags> <path> <pat1> [pat2 ...]`  | `Glob::union`     | `match` / `no-match` / `err:<Kind>`             |
//!
//! ## Byte escaping
//!
//! `\\` decodes to a single backslash, `\xHH` to the raw byte `0xHH`; every
//! other byte is itself. A PATTERN crosses Rust's `&str` API, so its decoded
//! bytes are run through `str::from_utf8` — invalid UTF-8 there is a driver
//! bug and is reported as `err:NonUtf8Pattern` rather than silently dropped.
//! A PATH stays raw `&[u8]`, so it may be arbitrary (non-UTF-8) bytes.

use globstar::{CompileOptions, DirMatch, Glob, GlobError};
use std::io::{self, BufRead, Write};

fn main() {
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
        let token = handle(&line);
        writeln!(out, "{token}").expect("stdout write");
    }
    out.flush().expect("stdout flush");
}

/// Dispatch one request line to its result token. Any structurally malformed
/// line yields `err:BadRequest` so a driver/protocol bug is loud, not a
/// silent false "agreement".
fn handle(line: &str) -> String {
    let cols: Vec<&str> = line.split('\t').collect();
    if cols.len() < 3 {
        return "err:BadRequest".to_string();
    }
    let cmd = cols[0];
    let opts = match parse_flags(cols[1]) {
        Some(o) => o,
        None => return "err:BadRequest".to_string(),
    };

    match cmd {
        // m <flags> <pattern> <path>
        "m" => {
            let pat = match decode_pattern(cols[2]) {
                Ok(p) => p,
                Err(t) => return t,
            };
            let path = unescape(cols[3]);
            match Glob::new_with(&pat, opts) {
                Ok(g) => is_match_token(g.is_match(&path)),
                Err(e) => err_token(&e),
            }
        }
        // d <flags> <pattern> <dir>
        "d" => {
            let pat = match decode_pattern(cols[2]) {
                Ok(p) => p,
                Err(t) => return t,
            };
            let dir = unescape(cols[3]);
            match Glob::new_with(&pat, opts) {
                Ok(g) => dir_token(g.match_dir(&dir)),
                Err(e) => err_token(&e),
            }
        }
        // u <flags> <path> <pat1> [pat2 ...]
        "u" => {
            if cols.len() < 4 {
                return "err:BadRequest".to_string();
            }
            let path = unescape(cols[2]);
            let mut patterns: Vec<String> = Vec::with_capacity(cols.len() - 3);
            for raw in &cols[3..] {
                match decode_pattern(raw) {
                    Ok(p) => patterns.push(p),
                    Err(t) => return t,
                }
            }
            match Glob::union_with(&patterns, opts) {
                Ok(g) => is_match_token(g.is_match(&path)),
                Err(e) => err_token(&e),
            }
        }
        _ => "err:BadRequest".to_string(),
    }
}

/// Two chars: dot then case_insensitive, each `0`/`1`.
fn parse_flags(s: &str) -> Option<CompileOptions> {
    let b = s.as_bytes();
    if b.len() != 2 {
        return None;
    }
    let dot = b[0] == b'1';
    let ci = b[1] == b'1';
    Some(CompileOptions::default().dot(dot).case_insensitive(ci))
}

/// Decode an escaped pattern field to a `String`. Patterns must be valid
/// UTF-8 (they cross `Glob::new_with`'s `&str` boundary); the JS driver
/// guarantees this by encoding via `TextEncoder`, so a failure here is a
/// driver bug surfaced as a distinct, never-silent token.
fn decode_pattern(field: &str) -> Result<String, String> {
    String::from_utf8(unescape(field)).map_err(|_| "err:NonUtf8Pattern".to_string())
}

fn is_match_token(m: bool) -> String {
    if m { "match" } else { "no-match" }.to_string()
}

fn dir_token(d: DirMatch) -> String {
    match d {
        DirMatch::Pruned => "pruned",
        DirMatch::Descend => "descend",
        DirMatch::Match => "match",
        DirMatch::DescendAndMatch => "descend-match",
    }
    .to_string()
}

/// Map a `GlobError` to the same kind string the JS `GlobError.kind` uses,
/// so `err:<Kind>` tokens compare 1:1 across runtimes.
fn err_token(e: &GlobError) -> String {
    use GlobError::*;
    let kind = match e {
        Empty => "Empty",
        TooLong { .. } => "TooLong",
        UnterminatedClass { .. } => "UnterminatedClass",
        UnterminatedBrace { .. } => "UnterminatedBrace",
        TrailingBackslash => "TrailingBackslash",
        BraceNestingTooDeep { .. } => "BraceNestingTooDeep",
        InvalidRange { .. } => "InvalidRange",
        EmptyPatternSet => "EmptyPatternSet",
        NegatedInUnion { .. } => "NegatedInUnion",
    };
    format!("err:{kind}")
}

/// Decode a byte-escaped wire field: `\\` → `\`, `\xHH` → byte `0xHH`,
/// anything else verbatim. Mirrors the JS driver's `escapeBytes`.
fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            match b[i + 1] {
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'x' if i + 3 < b.len() => {
                    let hi = hex(b[i + 2]);
                    let lo = hex(b[i + 3]);
                    match (hi, lo) {
                        (Some(h), Some(l)) => out.push((h << 4) | l),
                        // Malformed \x escape: keep the backslash literally so
                        // it round-trips deterministically instead of panicking.
                        _ => out.push(b'\\'),
                    }
                    i += 4;
                }
                _ => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
