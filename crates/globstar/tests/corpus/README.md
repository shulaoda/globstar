# Golden Test Corpus

This directory is the **executable form** of `references/spec/GLOB_SPEC.md`. Any
implementation claiming spec conformance must pass every test here.

## Files

Single-pattern `is_match` corpora: `corpus.txt` (core, spec-sourced),
`corpus-class.txt`, `corpus-case.txt`, `corpus-utf8.txt`,
`corpus-absolute.txt`, `corpus-comprehensive.txt`,
`corpus-realworld.txt`, `corpus-fast-glob.txt`,
`corpus-fast-glob-diff.txt`, `corpus-err.txt` (the parse errors the
others do not pin), plus the platform-gated `corpus-unix.txt` and
`corpus-windows.txt`.

Other surfaces: `corpus-dir.txt` (`match_dir`), `corpus-multi.txt` /
`corpus-multi-dir.txt` (unions).

The file lists are hard-coded in `tests/corpus.rs` and `scripts/verify.mjs` —
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

- **PATTERN** (column 1): the glob string; in `corpus-multi*.txt` a JSON array of
  glob strings.
- **PATH** (column 2): the path being matched.
- **EXPECTED** (column 3): expected result.
  - `corpus.txt` / `corpus-*.txt`: `match` | `no-match`
  - `corpus-dir.txt` / `corpus-multi-dir.txt`: `pruned` | `descend` | `match` |
    `descend-match` | `descend-all` | `descend-all-match`
  - any of these files: `err:Kind` — the pattern must fail to compile with
    that `GlobError` kind (PATH is then usually `-`)
- **FLAGS** (column 4, optional): `k=v[,k=v]` with keys `dot` and
  `case_insensitive`, values `true` or `false`, e.g. `dot=true`.

Nothing is skipped. A row whose pattern fails to compile where it expects
an answer is a failure, and a short row, an unknown EXPECTED word or an
unknown flag stops the run, in both runners (`tests/corpus.rs`,
`scripts/verify.mjs`).

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

## Provenance

Each case in `corpus.txt` / `corpus-dir.txt` is sourced
from a specific section of `GLOB_SPEC.md`. Cases pulled from external
corpora (fast-glob, real-world repos) live in separate files so the
spec-authoritative corpus stays clean.

## Scope

If a bug is found in the implementation, add a regression row here
**first**, then fix the code — the corpus is a live document.
