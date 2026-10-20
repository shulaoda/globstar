# 07 — Segment-Structured Matcher (SSM)

Source: `crates/globstar/src/engine/segment/` (Rust),
`packages/globstar/src/engine/segment/` (JS).

## The observation

Every construct in the dialect except `**` is segment-local (GLOB_SPEC
§8.2, §14.2): `*`, `?`, `[...]` cannot cross a separator, and
dot-protection keys on segment starts. After the globstar fold, `**`
appears in exactly four op forms, each of which denotes a
_segment-run absorber_. A pattern is therefore a graph of
segment-shaped elements — a line for a pattern without a
separator-crossing brace, a branching graph otherwise:

```
Element := Lit(bytes) | Wild(matcher) | G0 | G1
```

with `G0` absorbing ≥ 0 path segments and `G1` ≥ 1. (A `G0` that may not
begin with an empty segment is never needed: a `**/` absorbs empty
segments too, which is what makes the separators around it lenient,
§14.2.) Empty path segments are first-class
(`a//b` = `[a, "", b]`), which yields the strict-`Sep` semantics for
free as element-count equality.

A separator-crossing brace does not multiply the pattern out: the
compile walks the AST keeping one _group_ per distinct (boundary,
partial segment) reaching a point of the pattern, so `{a/,b/}x` is
three nodes with `x` shared, and `{a/,b/}` repeated k times is 2k+1
nodes, not 2^k sequences. The in-segment branches of such a brace are
walked as one choice (`{a,b,/}` as `{{a,b},/}`), and pending `**/`
count once (`**/**/` ≡ `**/`). Partial segments that differ in length
still give a group each: `a{a,/}` repeated k times is O(k²) nodes. The
walk counts its work and stops past 64 units per unit of pattern
weight, so such a shape fails with `TooComplex` instead of growing
(see Compile).

## Matching

Let m = element count, n = segment count. `is_match` enumerates the
paths through the graph as _chains_ while there are at most 64 of
them (one for a brace-free pattern, none when that one has several
globstars: it is the graph, whose NFA does the same work); each is
matched on its own, in pattern order, after two quick looks at the
path's ends: its first segment against a chain's leading literal, its
last against a literal suffix, an affix's prefix or a set's last
bytes. Before that, union members that differ only in the literal
ending one segment (`**/node_modules/**` and `**/.git/**`, `**/*.log`
and `**/*.tmp`, `src/**/*.ts` and `tests/**/*.ts`) are one chain with a
set in that segment: N chains each comparing a name become one
comparing a set.
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
  graph, one state per node, stepped once per _segment_. The active
  set is one u64 (int32 in JS) while the graph has at most 64 states
  (32 in JS), and per state the closure entered after consuming a
  segment is precomputed; in Rust a word slice does the same while
  those rows fit 4 KiB (about 170 states) and no position leads to
  more than 48 literals, each of which a dense step would test on its
  own. Past that (past 32 states in JS) the NFA is stepped sparsely:
  each position keeps its successors, the literal ones sorted, and
  the active set lists states and whole positions ("every successor
  of p entered"), so a step costs what it tests (one
  binary search for a position's literals) and the tables are linear
  in the graph. `match_dir`'s prefix bit is `active ∩ reach1`, where
  `reach1[s]` ⇔ some ≥ 1-segment continuation from s reaches accept
  (suffix satisfiability, precomputed). O(n · active), active ≤ 3 in
  practice for one pattern. A union member with several globstars
  carries its own small NFA (its nodes alone), so `**/node_modules/**`
  in a union costs what it would alone.
  `match_dir`'s all-below bit steps the graph NFA over wildcard-only
  segments until the set settles (§06 §2.5).

In-segment `Wild` matchers are classified at compile time:
`Affix` (prefix/suffix literals + `*`/`?` runs → length bounds + two
anchored compares), `AffixSet` (trailing all-literal alternation →
suffix product of at most 8, scanned in pattern order), `AffixIndex`
(the same past 8 — a list of any length, such as a factored union of
names, or a product of up to 1024 suffixes, within 64 KiB or four
times its nodes — sorted by (length, bytes) and binary-searched per
length), and `Generic` — a Thompson-lite NFA over the in-segment nodes
(u64/int32 active set up to 64/32 states, a word slice beyond), one
state per node: a `*` takes any byte and stays, with an ε-edge out.
Under dot=false a protected leading `.` is taken only by a literal `.`
or a positive class listing it: the entry closure of a dot-led segment
drops `*`, `?` and negated classes and passes no `*`. Every ε-edge
points to a later state, so one ascending sweep closes a set.
Per-state closures are kept while they fit 128 KiB (about 1,000
states) in both runtimes, about where the sweep becomes the faster
step; past that each
step closes its set as it goes, so memory stays linear.

## Compile

One walk over the AST (a `**` followed by `/` is a pending `**/`, a
`**` ending a brace branch reads the `/` after the brace as its own):
groups → graph nodes → classify each segment; then the chains (paths) are enumerated and the NFA tables
laid out. No subset construction, no byte-class tables; two hash
maps: one lets a brace branch reuse a node another branch already
made, one finds a group by its key past the first 16 (JS scans its
first 16 nodes before it keys them). Every pattern
the parser accepts compiles unless its separator-crossing braces make
its segments multiply: the walk counts its work — a unit per node and
edge, plus each group it copies and each segment it closes by their
size — and fails with `TooComplex` past max(2^20, 64 × the pattern's
weight). Ordinary patterns and big unions need one to three units
per weight, and a walk without a crossing brace has no budget at
all. The other limits are the parser's — 64 KiB of pattern, 32 levels
of brace nesting, the brace-expansion budget — and the 64-chain,
8-suffix, suffix-product, closure and dense-row thresholds pick a
representation, never reject one.

## Complexity and safety

Compile is linear in the pattern: a brace-free pattern walks its nodes
once, a separator-crossing brace adds one node per branch rather than
a product, and the walk's work is capped as above. Every table built
from the graph — chains, suffix products, in-segment closures, NFA
rows — has a size budget past which a linear representation takes
over, so memory is linear too. Match O(n·m) worst case — the NFA over
the graph is the non-backtracking floor that rules out ReDoS — and
O(n) for a chain with ≤ 1 globstar. `match_dir` adds at most 65
wildcard-only segments for the all-below proof (64 and the step that
shows the set has settled) and 4096 element
compares for a chain's tail tokens; past them it gives the safe
answer (GLOB_SPEC §15.5). The engine answers `is_match`, `match_dir`,
and `static_prefixes` natively.

## JS dual-mode execution

The JS port runs the same algorithm directly on JS strings
(`charCodeAt`/`startsWith`/`endsWith`/`indexOf` intrinsics, zero
per-call allocation). Byte-exactness is preserved by construction:
literal compares and `*`/globstar absorption are UTF-16-unit-count
independent, and the only counting constructs — `?` (one byte) and
negated classes (one byte) — read a non-ASCII segment's UTF-8 length,
or run on that one segment's bytes, so nothing is matched twice.
Patterns containing non-ASCII bytes compile to byte mode outright. The string↔byte equivalence is fuzzed by
`packages/globstar/tests/string-mode.mjs`.
