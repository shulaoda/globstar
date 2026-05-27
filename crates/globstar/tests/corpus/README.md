# Golden Test Corpus

This directory is the **executable form** of `spec/GLOB_SPEC.md`. Any
implementation claiming spec conformance must pass every test here.

## Files

- `corpus.txt` — core `is_match` cases (hand-authored, ~250 rows)
- `corpus-dir.txt` — `match_dir` pruning cases (~50 rows)
- `corpus-err.txt` — parse-error cases (~30 rows)
- `corpus-realworld.txt` — patterns observed in real-world projects
- `corpus-fast-glob.txt` — cases imported from oxc-project/fast-glob
- `corpus-utf8.txt` — UTF-8 / multibyte path and pattern coverage

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

When FLAGS is omitted, the spec defaults apply:

- `dot = false`
- case-sensitive
- brace / globstar always enabled (no toggle — see decision D-006).

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

## Scope target

Initial scale: ~300 hand-authored rows across the three spec files, plus
whatever the external corpora contribute. If a bug is found in the
implementation, add a regression row here **first**, then fix the code —
the corpus is a live document.
