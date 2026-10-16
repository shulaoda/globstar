# 07 — Segment-Structured Matcher (SSM)

Source: `crates/globstar/src/engine/segment/` (Rust),
`packages/globstar/src/engine/segment/` (JS).

## The observation

Every construct in the dialect except `**` is segment-local (GLOB_SPEC
§6.2, §12.3): `*`, `?`, `[...]` cannot cross a separator, and
dot-protection keys on segment starts. After the globstar fold, `**`
appears in exactly four op forms, each of which denotes a
_segment-run absorber_. A pattern is therefore a graph of
segment-shaped elements — a line for a pattern without a
separator-crossing brace, a branching graph otherwise:

```
Element := Lit(bytes) | Wild(matcher) | G0 | G0Strict | G1
```

with `G0` absorbing ≥ 0 path segments, `G1` ≥ 1, and `G0Strict` a G0
whose absorbed run may not begin with an empty segment (spliced
strict-`Sep` + OSS boundaries). Empty path segments are first-class
(`a//b` = `[a, "", b]`), which yields the strict-`Sep` semantics for
free as element-count equality.

A separator-crossing brace does not multiply the pattern out: the
compile walks the ops keeping one _group_ per distinct (boundary,
partial segment) reaching a point of the pattern, so `{a,b}/x` is
three nodes with `x` shared, and `{a,b}/` repeated k times is 2k+1
nodes, not 2^k sequences.

## Matching

Let m = element count, n = segment count. `is_match` enumerates the
paths through the graph as _chains_ while there are at most 64 of
them (one for a brace-free pattern); each is matched on its own, in
pattern order, with a quick last-segment suffix check between them.
Past 64 paths the NFA over the whole graph runs instead. `match_dir`
goes the same way: one chain with at most one globstar — the common
case — answers it by the same anchored compares, reading the tokens
the NFA would hold after the directory off the chain, and such a
matcher keeps no NFA at all. Only a matcher whose chains cannot serve
(none, several, or one with several globstars) builds the NFA.

- **Zero G**: positional — n must equal m, element i consumes segment
  i. O(n).
- **One G at g** (the dominant real-world shape): the head `0..g` and
  tail `g+1..m` are both _anchored_ — head against the first g
  segments (a literal is compared in place, no segment scan), tail
  against the last m−g−1 segments right-to-left. The absorber takes
  whatever remains; arity (G1 ≥ 1, overlap) is byte-position
  arithmetic, and the `dot=false` rule is one scan of the absorbed
  byte range for `(^|/)\.`. O(n), no search, no backtracking.
- **Several Gs / a union's `match_dir` / > 64 paths**: an NFA over the
  graph,
  one state per node (two for `G0Strict`/`G1`), stepped once per
  _segment_. The active set is one u64 (int32 in JS) while the graph
  has at most 64 states (31 in JS) and a word slice beyond, so no
  pattern is too large. Per state the closure entered after consuming
  a segment is precomputed; `match_dir`'s prefix bit is `active ∩
reach1`, where `reach1[s]` ⇔ some ≥ 1-segment continuation from s
  reaches accept (suffix satisfiability, precomputed). O(n · active),
  active ≤ 3 in practice for one pattern. A chain with several
  globstars carries its own small NFA (its nodes alone) so a union
  member like `**/node_modules/**` costs what it would alone.
  `match_dir`'s all-below bit steps the graph NFA over wildcard-only
  segments until the set settles (§06 §2.5).

In-segment `Wild` matchers are classified at compile time:
`Affix` (prefix/suffix literals + `*`/`?` runs → length bounds + two
anchored compares), `AffixSet` (trailing all-literal alternation →
suffix product of at most 8, scanned in pattern order), `AffixIndex`
(the same past 8 — a list of any length, such as a factored union of
names, or a product of up to 1024 suffixes, within 64 KiB or four
times its ops — sorted by (length, bytes) and binary-searched per
length), and `Generic` — a Thompson-lite NFA over the in-segment ops
(u64/int32 active set up to 64/32 states, a word slice beyond;
offset-0 dot gates realizing the DotGuard semantics exactly). Every
ε-edge points to a later state, so one ascending sweep closes a set.
Per-state closures are kept while they fit 1 MiB in Rust and 128 KiB
in JS, about where the sweep becomes the faster step; past that each
step closes its set as it goes, so memory stays linear.

## Compile

One walk over the ops: groups → graph nodes → classify each
segment; then the chains (paths) are enumerated and the NFA tables
laid out. No subset construction, no byte-class tables; the one
hash map, in the Rust builder only, lets a brace branch reuse a
node another branch already made. Every pattern the parser accepts
compiles; the only limits are the parser's — 64 KiB of pattern, 32
levels of brace nesting, the brace-expansion budget — and the
64-chain, 8-suffix, suffix-product and closure-budget thresholds pick
a representation, never reject one.

## Complexity and safety

Compile O(|pattern| + edges) — linear in the ops for a brace-free
pattern, and a separator-crossing brace adds one node per branch
rather than a product. Match O(n·m) worst case — the NFA over the
graph is the non-backtracking floor that rules out ReDoS — and O(n) for
a chain with ≤ 1 globstar. The engine answers `is_match`, `match_dir`,
and `static_prefixes` natively.

## JS dual-mode execution

The JS port runs the same algorithm directly on JS strings
(`charCodeAt`/`startsWith`/`endsWith`/`indexOf` intrinsics, zero
per-call allocation). Byte-exactness is preserved by construction:
literal compares and `*`/globstar absorption are UTF-16-unit-count
independent, and the only counting constructs — `?` (one byte) and
negated classes (one byte) — **bail to byte mode** when they meet a
char code > 0x7F. Patterns containing non-ASCII bytes compile to
byte mode outright. The string↔byte equivalence is fuzzed by
`packages/globstar/tests/string-mode.mjs`.
