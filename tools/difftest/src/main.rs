//! The Rust side of the JS↔Rust differential fuzzer (`scripts/fuzz.mjs`):
//! one request per line on stdin, one result token per line on stdout.
//!
//! Tab-separated. The first field is the command, the second two flag
//! chars, `dot` then `case_insensitive` (`0`/`1`); every following field
//! is a byte-escaped token (`\\` a backslash, `\xHH` the byte `0xHH`).
//!
//! | line                                  | op                                       | output tokens                      |
//! |---------------------------------------|------------------------------------------|------------------------------------|
//! | `m <flags> <pattern> <path>`          | `Glob::is_match`                         | `match` / `no-match` / `err:<Kind>` |
//! | `n <flags> <pattern> <path>`          | `force_nfa` + `is_match`                 | as `m`                             |
//! | `d <flags> <pattern> <dir>`           | `Glob::match_dir`                        | `pruned` / `descend` / `match` / `descend-match` / `descend-all` / `descend-all-match` / `err:<Kind>` |
//! | `u <flags> <path> <pat1> [pat2 ...]`  | `Glob::union` + `is_match`               | as `m`                             |
//! | `un <flags> <path> <pat1> [pat2 ...]` | `Glob::union` + `force_nfa` + `is_match` | as `m`                             |
//! | `ud <flags> <dir> <pat1> [pat2 ...]`  | `Glob::union` + `match_dir`              | as `d`                             |
//! | `s <flags> <pat1> [pat2 ...]`         | `Glob::static_prefixes` (union when >1)  | `px:<escaped,sorted>` / `err:<Kind>` |
//!
//! A pattern crosses the `&str` API: one that is not UTF-8 is a driver
//! bug, reported as `err:NonUtf8Pattern`. A path stays raw bytes.

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

/// A malformed line is `err:BadRequest`, never a silent agreement.
fn handle(line: &str) -> String {
    let cols: Vec<&str> = line.split('\t').collect();
    if cols.len() < 3 {
        return "err:BadRequest".to_string();
    }
    let cmd = cols[0];
    let opts = match parse_flags(cols[1]) {
        Some(o) => o.force_nfa(matches!(cmd, "n" | "un")),
        None => return "err:BadRequest".to_string(),
    };

    match cmd {
        "m" | "n" | "d" => {
            let (pat, input) = match (decode_pattern(cols[2]), cols.get(3)) {
                (Ok(p), Some(f)) => (p, unescape(f)),
                _ => return "err:BadRequest".to_string(),
            };
            match Glob::new_with(&pat, opts) {
                Ok(g) => answer(cmd, &g, &input),
                Err(e) => err_token(&e),
            }
        }
        "u" | "un" | "ud" => {
            if cols.len() < 4 {
                return "err:BadRequest".to_string();
            }
            let input = unescape(cols[2]);
            let mut patterns: Vec<String> = Vec::with_capacity(cols.len() - 3);
            for raw in &cols[3..] {
                match decode_pattern(raw) {
                    Ok(p) => patterns.push(p),
                    Err(t) => return t,
                }
            }
            match Glob::union_with(&patterns, opts) {
                Ok(g) => {
                    let query = match cmd {
                        "u" => "m",
                        "un" => "n",
                        _ => "d",
                    };
                    answer(query, &g, &input)
                }
                Err(e) => err_token(&e),
            }
        }
        "s" => {
            let mut patterns = Vec::with_capacity(cols.len() - 2);
            for raw in &cols[2..] {
                match decode_pattern(raw) {
                    Ok(p) => patterns.push(p),
                    Err(t) => return t,
                }
            }
            let compiled = if patterns.len() == 1 {
                Glob::new_with(&patterns[0], opts)
            } else {
                Glob::union_with(&patterns, opts)
            };
            match compiled {
                Ok(g) => {
                    let mut px: Vec<String> =
                        g.static_prefixes().iter().map(|p| escape(p)).collect();
                    px.sort();
                    format!("px:{}", px.join(","))
                }
                Err(e) => err_token(&e),
            }
        }
        _ => "err:BadRequest".to_string(),
    }
}

fn answer(query: &str, g: &Glob, input: &[u8]) -> String {
    match query {
        "m" | "n" => is_match_token(g.is_match(input)),
        _ => dir_token(g.match_dir(input)),
    }
}

fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b == b'\\' {
            out.push_str("\\\\");
        } else if (0x20..=0x7e).contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn parse_flags(s: &str) -> Option<CompileOptions> {
    let b = s.as_bytes();
    if b.len() != 2 {
        return None;
    }
    let dot = b[0] == b'1';
    let ci = b[1] == b'1';
    Some(CompileOptions::default().dot(dot).case_insensitive(ci))
}

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
        DirMatch::DescendAll => "descend-all",
        DirMatch::DescendAllAndMatch => "descend-all-match",
    }
    .to_string()
}

/// The variant's name, as JS `GlobError.kind` spells it; a union member's
/// error is its own kind in JS, so `InMember` reads through to it.
fn err_token(e: &GlobError) -> String {
    let debug = format!("{:?}", e.root());
    let kind = debug.split([' ', '{', '(']).next().unwrap_or(&debug);
    format!("err:{kind}")
}

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
                        // A malformed `\x`: a lone backslash stands in.
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
