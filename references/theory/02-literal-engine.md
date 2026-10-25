# 02. The Literal Engine

A pattern made of literals and separators only (`src/main.rs`) denotes one path, up to separators and case. The literal engine answers the three questions of the matcher by comparing bytes. Source: `crates/globstar/src/engine/literal.rs`; JS `packages/globstar/src/engine/literal.js`.

## 1. Representation

```
LiteralMatcher { literal: bytes, case_insensitive: bool }
```

`literal` is the pattern's bytes with every separator written as `/`. The JS matcher keeps the literal as a Latin-1 string, one character per byte; a literal that is ASCII, compared case-sensitively on a platform where `\` is not a separator, is matched by string equality alone (`exact`), and a non-ASCII path is converted to its UTF-8 bytes before any other comparison.

## 2. Byte comparison

One rule compares a pattern byte with a path byte (`bytes::pat_byte_eq`): a pattern `/` matches any byte of `Seps`; a pattern byte that is itself in `Seps` (an escaped `\` on Windows) matches nothing; any other byte matches itself, or either case of itself under `case_insensitive`. This rule is GLOB_SPEC §7.1 and §7.4 for a literal, and the literal engine applies no other.

## 3. `is_match`

A path matches when it has the literal's length and every byte matches by §2. The bytes are compared from the end (`ends_with_pattern`), where file names differ most, so a mismatch is found early.

## 4. `match_dir`

`prefix_len(d)` is the number of leading bytes of the directory `d` that the literal matches. Three cases:

```
n = prefix_len(d)
n = |d| = |literal|                       →  Match
d = ""  or  (n = |d| and literal[n] = '/') →  Descend
otherwise                                 →  Pruned
```

The literal denotes one path, so `all_below` never holds and the covering results are never returned. A directory that is a whole segment-prefix of the literal has a match below it; the root has one by definition (GLOB_SPEC §15.2); any other directory has none.

## 5. `static_prefixes`

The literal without its trailing `/`s is its own prefix; a literal that holds a separator byte of the platform (an escaped `\` on Windows) matches nothing and has no prefix (GLOB_SPEC §17).

## 6. Correctness and cost

`is_match` is `L(P) = {literal}` under the byte rule of §2, read directly. `match_dir` returns `Pruned` only when `d` is neither the literal nor a segment-prefix of it, and no path below such a `d` can be the literal. Every operation is one pass over at most `|literal|` bytes, and the matcher holds the literal and nothing else.

## 7. Source map

| Symbol                                                                     | File                                |
| -------------------------------------------------------------------------- | ----------------------------------- |
| `LiteralMatcher`, `is_match`, `match_dir`, `prefix_len`, `static_prefixes` | `engine/literal.rs`                 |
| `pat_byte_eq`, `ends_with_pattern`, `has_escaped_sep`                      | `engine/bytes.rs`                   |
| `LiteralMatcher`, `prefixLen`                                              | `packages/globstar/src/engine/literal.js` |
