# 07 — Segment-Structured Matcher (SSM)

Source: `crates/globstar-segment/src/engine/` (Rust),
`packages/globstar-segment/src/engine.js` (JS).
Design + errata: `references/decisions/segment-engine-design.md`.

## The observation

Every construct in the dialect except `**` is segment-local (§GLOB_SPEC
6.2, 12.3): `*`, `?`, `[...]` cannot cross a separator, and
dot-protection keys on segment starts. After the globstar fold, `**`
appears in exactly four op forms, each of which — including the
splice-composed forms produced by fork expansion — denotes a
*segment-run absorber*. A pattern is therefore a linear sequence of
segment-shaped elements:

```
Element := Lit(bytes) | Wild(matcher) | G0 | G0Strict | G1
```

with `G0` absorbing ≥ 0 path segments, `G1` ≥ 1, and `G0Strict` a G0
whose absorbed run may not begin with an empty segment (spliced
strict-`Sep` + OSS boundaries). Empty path segments are first-class
(`a//b` = `[a, "", b]`), which yields the strict-`Sep` semantics for
free as element-count equality.

## Matching

Let m = element count, n = segment count.

- **Zero G**: positional — n must equal m, element i consumes segment
  i. O(n).
- **One G at g** (the dominant real-world shape): the head `0..g` and
  tail `g+1..m` are both *anchored* — head against the first g
  segments (one pre-joined sep-aware memcmp when all-literal), tail
  against the last m−g−1 segments right-to-left. The absorber takes
  whatever remains; arity (G1 ≥ 1, overlap) is byte-position
  arithmetic, and the `dot=false` rule is one scan of the absorbed
  byte range for `(^|/)\.`. O(n), no search, no backtracking.
- **Several Gs / `match_dir`**: an NFA over element positions, one
  state per element (two for `G0Strict`/`G1`), active set in a single
  u64 (u32 in JS), stepped once per *segment*. ε-closures ("absorb
  zero") are precomputed; `match_dir`'s prefix bit is `active ∩
  reach1`, where `reach1[s]` ⇔ some ≥ 1-segment continuation from s
  reaches accept (suffix satisfiability, precomputed). O(n · active),
  active ≤ 3 in practice.

In-segment `Wild` matchers are classified at compile time:
`Affix` (prefix/suffix literals + `*`/`?` runs → length bounds + two
anchored compares), `AffixSet` (trailing all-literal alternation →
suffix product, capped at 16), and `Generic` — a Thompson-lite NFA
over the in-segment ops (≤ 64 states Rust / 32 JS, u64/u32 active
set, memoized ε-closures, offset-0 dot gates realizing the DotGuard
semantics exactly).

## Compile

One linear pass: fork expansion (separator-crossing braces, capped at
64 sequences) → segmentize → classify. No subset construction, no
hash maps, no byte-class tables. Patterns outside the model
(`a{**,x}b`-style glued absorbers, escaped separators inside
literals, budget overflows — ~0.5% of the corpus, all degenerate stress
shapes) return the program unchanged and fall through to the shared
`PikeVm`, the total O(n·m) engine.

## Complexity and safety

Compile O(|pattern|) with fork/product caps; match O(n·m) worst case
(same bound as the Pike VM — the ReDoS floor of ADR-007 is
unchanged), O(n) for ≤ 1 globstar. The engine answers `is_match`,
`match_dir`, and `static_prefixes` natively.

## JS dual-mode execution

The JS port runs the same algorithm directly on JS strings
(`charCodeAt`/`startsWith`/`endsWith`/`indexOf` intrinsics, zero
per-call allocation). Byte-exactness is preserved by construction:
literal compares and `*`/globstar absorption are UTF-16-unit-count
independent, and the only counting constructs — `?` (one byte) and
negated classes (one byte) — **bail to byte mode** when they meet a
char code > 0x7F. Patterns containing non-ASCII bytes compile to
byte mode outright. The string↔byte equivalence is fuzzed by
`packages/globstar-segment/tests/string-mode.mjs`.
