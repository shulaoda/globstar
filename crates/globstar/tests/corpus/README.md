# Golden Test Corpus

This directory is the **executable form** of `spec/GLOB_SPEC.md`. Any
implementation claiming spec conformance must pass every test here.

## Files

Single-pattern `is_match` corpora: `corpus.txt` (core, spec-sourced),
`corpus-class.txt`, `corpus-case.txt`, `corpus-utf8.txt`,
`corpus-absolute.txt`, `corpus-comprehensive.txt`,
`corpus-realworld.txt`, `corpus-fast-glob.txt`,
`corpus-fast-glob-diff.txt`, plus the platform-gated `corpus-unix.txt`
and `corpus-windows.txt`.

Other surfaces: `corpus-dir.txt` (`match_dir`), `corpus-multi.txt` /
`corpus-multi-dir.txt` (unions), `corpus-err.txt` (parse errors).

The file lists are hard-coded in `tests/corpus.rs` and `verify.mjs` —
a new corpus file must be registered in BOTH, or it runs nowhere.

## Format

Every corpus file shares the same simple TSV layout:

```
# Line comments start with `#`
# Blank lines are ignored

## group.name         ← section marker, descriptive only
PATTERN<TAB>PATH<TAB>EXPECTED[<TAB>FLAGS]
```

### Fields

- **PATTERN** (column 1): the glob string.
- **PATH** (column 2): the path being matched.
- **EXPECTED** (column 3): expected result.
  - `corpus.txt` / `corpus-*.txt`: `match` | `no-match`
  - `corpus-dir.txt`: `pruned` | `descend` | `match` | `descend-match`
  - `corpus-err.txt`: an `ErrorKind` name (e.g. `UnterminatedClass`)
- **FLAGS** (column 4, optional): `k=v[,k=v]`, e.g. `dot=true`.

### Escape rules

Inside the PATTERN and PATH fields the following escapes are recognized:

- `\\` → literal `\`
- `\t` → tab
- `\n` → newline
- every other byte is taken literally

This lets a pattern embed `\\*` (an escaped star, matching a literal `*`)
without colliding with the TSV tab separator.

**Empty fields**: two consecutive tabs denote the empty string. For
example, `*<TAB><TAB>match` is "pattern `*` matches the empty path".

### Default flags

When FLAGS is omitted, the matcher defaults apply: `dot = true` for the
`is_match` and multi corpora, `dot = false` for the dir corpora (the
walker-layer default), case-sensitive everywhere.

## Minimal driver sketch (reference)

```rust
#[test]
fn corpus() {
    let text = std::fs::read_to_string("tests/corpus/corpus.txt").unwrap();
    let mut n_ok = 0;
    let mut n_fail = 0;
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') { continue; }
        let fields: Vec<&str> = line.split('\t').collect();
        let pattern  = unescape(fields[0]);
        let path     = unescape(fields[1]);
        let expected = fields[2];
        let flags    = fields.get(3).copied().unwrap_or("");
        // ... parse flags, compile, run is_match, compare
    }
    assert_eq!(n_fail, 0);
}
```

`corpus-dir.txt` and `corpus-err.txt` follow the same shape but call
`match_dir` or assert on `Glob::new().is_err()` respectively.

## Provenance

Each case in `corpus.txt` / `corpus-dir.txt` / `corpus-err.txt` is sourced
from a specific section of `GLOB_SPEC.md`. Cases pulled from external
corpora (fast-glob, real-world repos) live in separate files so the
spec-authoritative corpus stays clean.

## Scope

If a bug is found in the implementation, add a regression row here
**first**, then fix the code — the corpus is a live document.
