# 02 — Segment Engine

Source: `crates/globstar/src/engine/segment/` (Rust),
`packages/globstar/src/engine/segment/` (JS).

## The observation

Every construct in the dialect except `**` is segment-local (GLOB_SPEC
§2.3): `*`, `?`, `[...]` cannot cross a separator, and dot-protection
keys on segment starts. A `**` owns whole segments (a leading `**/`, a
middle `/**/`, a trailing `/**`, or the whole pattern), and the compile
walk turns each one into a _segment-run absorber_. A pattern is therefore a graph of
segment-shaped elements — a line for a pattern without a
separator-crossing brace, a branching graph otherwise:

```
Element := Lit(bytes) | Wild(matcher) | G0 | G1
```

with `G0` absorbing ≥ 0 path segments and `G1` ≥ 1. (A `**/` absorbs
empty segments too, which is what makes the separators around it
lenient, §9.5.) Empty path segments are first-class
(`a//b` = `[a, "", b]`), which yields the strict-`Sep` semantics for
free as element-count equality.

A separator-crossing brace does not multiply the pattern out: the
compile walks the AST keeping one _group_ per distinct (boundary,
partial segment) reaching a point of the pattern, so `{a/,b/}x` is
three nodes with `x` shared, and `{a/,b/}` repeated k times is 2k+1
nodes, not 2^k sequences. The in-segment branches of such a brace are
walked as one choice (`{a,b,/}` as `{{a,b},/}`), and pending `**/`
count once (`**/**/` ≡ `**/`), except that one whose group gathered
tails from several branches becomes a `G0` of its own when the next is
read: `{**,a}/` repeated k times keeps about 5k edges, not one group
whose tails pile up into k² of them. Partial segments that differ in
length still give a group each: `a{a,/}` repeated k times is O(k²)
nodes. And a node takes an edge from each tail of its group, so
branches that may be empty, side by side, give O(k²) edges among O(k)
nodes (`{a/,}` repeated: each `a` follows every one before it). The
walk counts its work and its edges and stops past 64 units, or 16
edges, per unit of pattern weight, so such a shape fails with
`TooComplex` instead of growing (see Compile).

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
and `**/*.tmp`, `src/**/*.ts` and `tests/**/*.ts`) become one path with
a set in that segment (one chain, or the graph itself when it is the
only path): N chains each comparing a name become one comparing a set.
The paths of literals only — a union of file names — are kept apart as
one sorted set, searched once, up to 4096 of them. A path with two
globstars around a block of elements (`**/x/**`, `src/**/{a,b}/**`) is
walked directly too: head from the front, tail from the back, the block
found among the segments between with the dot rule. Past 64 wildcard
paths or 4096 literal ones, or when a member has more than two
globstars, the NFA over the whole graph runs instead.

A union of more than 10 chains does not try them all: a _chain index_
files each chain under the literal strings one of its anchored
segments must have — its last element's segment (a name, `.DS_Store`;
the end of an extension, `*.log`; the start of a prefix, `._*`; a
literal it holds, `*.so.*`), else its first's, else the first of its
block, any segment but the last — read off that segment's nodes, small
classes and braces expanded (`[Dd]ebug`, `*.py[cod]`), while the graph
is built. The literal set is filed under its paths' first segments. A
key is its length and outer four bytes, a lookup one hash probe, and a
64-bit sketch of the outer bytes skips the lookups that cannot hit; a
path looks up its last segment, then its first, then the others, and
walks each chain picked, without the gates: most were picked by the
segment a gate reads, and the walk turns the others down about as
fast. A chain no key files is tried on every call. The walk decides; the index only chooses
which chains to ask, so a 600-member .gitignore costs a few lookups and
one or two walks, not 60. Ten chains or fewer stay a scan: their gates
cost less than the lookups. So does a union of 64 wildcard chains and
the literal set, one chain more than a word has bits. The keys are held
to one per unit of pattern weight, never fewer than 4096: a segment
whose keys pass what is left has none, and a union whose chains would
file more entries than that (64 chains sharing a list of 12,000 names)
is scanned too, so the index grows with the pattern, not with its
chains times their keys. `match_dir` goes the same way: one chain with at most one
globstar — the common case — answers it by the same anchored compares,
reading the tokens the NFA would hold after the directory off the
chain, and such a matcher keeps no NFA at all. Only a matcher whose
chains cannot serve (none, several, or one with more than one
globstar) builds the NFA.

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
  practice for one pattern. A union member with two globstars around a
  block (`**/node_modules/**`) is walked directly like a line, so it
  costs what it would alone; only a member with more sends the union
  to this NFA.
  `match_dir`'s all-below bit steps the graph NFA over wildcard-only
  segments until the set settles (§04 §2.5).

In-segment `Wild` matchers are classified at compile time:
`Affix` (prefix/suffix literals + `*`/`?` runs → length bounds + two
anchored compares), `Suffixes` (trailing all-literal alternation — a
list of any length, such as a factored union of names, or a product
of up to 1024 suffixes, within 64 KiB or four times its nodes — the
suffixes grouped by their last byte, so a segment is compared with the
few that end as it does, in pattern order), and `Generic` — a Thompson-lite NFA over the in-segment nodes
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
edge, each token it adds to a group, each group it copies or keys by
all its nodes and each segment it closes by their size, and the tables
a wildcard segment builds (its suffix list, its NFA's states and
closures) by their bytes — and fails with `TooComplex` past
max(2^20, 64 × the pattern's weight), where a literal weighs its
bytes, a class one plus its ranges, a brace one plus its branches (an
empty one weighs one) and any other token one. Ordinary patterns and big unions need two to four units
per weight, and a walk without a crossing brace has no budget at
all. It also fails once the graph would keep more than max(4096, 16 ×
the weight) edges: a sparse step walks the successors of what is
active, so this holds a segment's cost to the pattern's size, where
ordinary patterns keep at most two edges per weight and two wide
braces side by side well under sixteen. The other limits
are the parser's — 64 KiB of pattern, 32 levels
of brace nesting, the brace-expansion budget — and the chain,
literal-set, suffix-group, suffix-product, closure and dense-row
thresholds pick a representation, never reject one.

## Complexity and safety

Compile is linear in the pattern: a brace-free pattern walks its nodes
once, a separator-crossing brace adds one node per branch rather than
a product, and the walk's work is capped as above. Every table built
from the graph — chains, suffix products, in-segment closures, NFA
rows — has a size budget past which a linear representation takes
over, so memory is linear too. Match O(n·m) worst case, m the graph's
nodes and edges, both within max(4096, 16 × the weight) — the NFA over
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
