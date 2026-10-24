# 02 — Segment Engine

Source: `crates/globstar/src/engine/segment/` (Rust),
`packages/globstar/src/engine/segment/` (JS). The JS port is a twin:
the same layers in the same files, the same thresholds, the same
answers; where it differs, §12 says so.

## 1. The idea

Every construct in the dialect except `**` is segment-local (GLOB_SPEC
§2.3): `*`, `?` and `[...]` cannot cross a separator, and
dot-protection keys on segment starts. A `**` owns whole segments (a
leading `**/`, a middle `/**/`, a trailing `/**`, or the whole
pattern). A pattern is therefore a graph of segment-shaped elements,
and a path is matched one segment per element:

| Element        | Takes                                                  | Comes from                                   |
| -------------- | ------------------------------------------------------ | -------------------------------------------- |
| `Lit(bytes)`   | one segment equal to the bytes                         | `src`, `.git`, an empty segment              |
| `Wild(m)`      | one segment the in-segment matcher `m` accepts (§7)    | `*.ts`, `[Dd]ebug`, `{a,b}?`                 |
| `G0`           | ≥ 0 segments, empty ones too                           | `**/`, `/**/`, a whole-pattern `**`          |
| `G1`           | ≥ 1 segment                                            | a trailing `/**`: `a/**` takes `a/`, not `a` |
| `Join`         | none: it stands for its successors                     | the walk, to share edges (§3.3)              |
| `Piece(entry)` | part of a segment, matched with the pieces glued to it | the walk, past the copy room (§3.4)          |

Empty path segments are first-class (`a//b` is `[a, "", b]`), which
gives the strict-`Sep` semantics as element-count equality. A `**/`
absorbs empty segments too, which is what makes the separators around
it lenient (GLOB_SPEC §9.5).

A pattern without a separator-crossing brace is one path, a line of
elements; a crossing brace makes the graph branch. Most patterns are
lines, and most of the engine's layers exist so that those, and unions
of them, never reach the general machinery.

## 2. The layers at a glance

Compiling is one pass per layer, each linear in the pattern:

1. **Parse** (`parse/`): the AST. A union's members are factored into
   one body first (`union.rs`, §8).
2. **Facts** (`facts.rs`): the suffix prefilter (§03) and the static
   prefixes (§04), read off the AST as written.
3. **Graph walk** (`graph.rs`): the AST to the element graph (§3).
   Each closed segment is compiled to its element (`wild.rs`, §7); the
   segments a brace cuts into pieces become one `GlueNfa`
   (`seg_nfa.rs`, §7.4).
4. **Chains** (`chain.rs`): the paths through the graph, while they
   are few and simple (§4).
5. **Picker**: how a union tries its chains — each after its gate
   (`chain.rs`) or only those the chain index picks
   (`chain_index.rs`) (§5).
6. **Graph NFA** (`graph_nfa.rs`): built unless one line serves alone (§6).

What a call runs then depends only on what the compile kept:

| The matcher keeps                    | `is_match` (after the prefilter)                                                | `match_dir`              |
| ------------------------------------ | ------------------------------------------------------------------------------- | ------------------------ |
| one line (a chain with ≤ 1 globstar) | the line's anchored compares (§4.1)                                             | read off the line (§4.4) |
| chains, a picker and an NFA          | the literal set, then the chains: each after its gate, or those the index picks | the NFA (§6)             |
| an NFA alone                         | the NFA                                                                         | the NFA                  |

The graph keeps no chains — and the NFA answers `is_match` too — when
a brace cut a segment into pieces, when there are more than 64
wildcard paths or 4,096 literal ones (or their bytes pass 64 KiB and
four times the pattern's weight), or when a path has more than two
globstars, or two with nothing between them.

## 3. The graph

One walk over the AST builds the graph. It keeps one _group_ per
distinct (boundary, partial segment) reaching a point of the pattern:
the nodes awaiting a successor (its _tails_), the boundary just
crossed, and the in-segment nodes read since. Groups that agree on all
of that continue as one, so their continuations are the same nodes.

### 3.1 Braces

A separator-crossing brace does not multiply the pattern out: each
branch is walked from the groups before it, and the groups they leave
merge again. `{a/,b/}x` is three nodes with `x` shared, and `{a/,b/}`
repeated k times is 2k + 1 nodes, not 2^k sequences. The in-segment
branches of such a brace are walked as one choice (`{a,b,/}` as
`{{a,b},/}`, `regroup`). A node is reused when its element and
predecessors already exist, which turns a union of names into a trie.
Two hash maps serve the walk: one finds a node to reuse (JS scans its
first 16 nodes before it keys them), one finds a group by its key past
the first 16 groups.

A brace that crosses no separator stays inside its segment and is the
in-segment matcher's (§7).

### 3.2 Globstars

The parser reads a `**` beside text as a star. One beside a separator,
a brace or an edge of the pattern is decided per group, as the brace
expansions reach it (GLOB_SPEC §9.1): a group at a boundary that reads
`**` holds it until the next token — a `/` makes it a pending `**/`,
the end a bare `**`, anything else a star. Pending `**/` count once
(`**/**/` ≡ `**/`), except that one whose group gathered tails from
several branches becomes a `G0` of its own when the next is read:
`{**,a}/` repeated k times keeps about 5k edges, not one group whose
tails pile up into k² of them. The empty expansions of a brace beside
a `**` go on as a _shadow_ group while the `**` may still be a
globstar (`{a,}**`, `**{,a}/`), and end once text decides it is a
star.

### 3.3 Joins

A node takes an edge from each tail of its group, and a group is
copied into every branch of a crossing brace. So a group of k tails
entering a brace of m branches first gathers its tails into one `Join`
node when k·m > k + m: the branches share its one edge instead of
taking k each. Branches that may be empty, side by side, then keep a
few edges per brace: `{a/,}` repeated k times is about 2k nodes and 4k
edges, where each `a` would otherwise follow every one before it
(k²/2 edges). No NFA set holds a join, and a chain leaves it out.

### 3.4 The copy room and pieces

A segment a brace's edge falls inside is copied into each branch:
`x{a/,b}y` reads the segments `xa` and `xby`, and text after the brace
goes onto every segment the branches leave open. Partial segments that
differ in length still give a group each, so `a{a,/}` repeated k times
would copy O(k²). The copies are paid from a room of one unit per unit
of pattern weight, never less than 16,384 (`COPY_PER_WEIGHT`,
`MIN_COPY_ROOM`), where a literal weighs its bytes, a class one plus
its ranges, a brace one plus its branches (an empty one weighs one)
and any other token one.

Past the room, the walk cuts each open segment into a _piece_ at the
brace's edge, and the pieces after the brace are _glued_ to it: `x`,
then `a` and `b` glued to `x`, and `y` to `b`. A piece is never
copied, so the walk makes every node, edge and token a bounded number
of times and the graph is linear in the pattern. Ordinary patterns and
big unions never reach the room, so their segments stay whole, with
their chains and literal sets; a graph with pieces keeps no chains,
and its sparse NFA owns the glue (§7.4).

## 4. Chains

`Graph::chains` enumerates the paths through the graph, depth first in
pattern order, joins left out. A path of literals only joins the
literal set; every other path becomes a chain whose shape is fixed by
where its globstars are. Let m be a chain's elements and n the path's
segments.

### 4.1 A line: at most one globstar

- **No globstar**: positional — n must equal m, element i takes
  segment i. O(n).
- **One globstar at g** (the dominant real-world shape): the head
  `0..g` is anchored against the first g segments (a literal is
  compared in place, no segment scan), the tail `g+1..m` against the
  last m−g−1 segments, right to left. The globstar takes whatever
  remains: its arity (`G1` ≥ 1, overlap) is byte-position arithmetic,
  and the `dot=false` rule is one scan of the absorbed bytes for
  `(^|/)\.`. O(n), no search, no backtracking.

### 4.2 A block: two globstars around elements

A path with two globstars around a block (`**/x/**`,
`src/**/{a,b}/**`) is walked directly too: the head from the front,
the tail from the back, the block found among the segments between,
moving only right, with the dot rule checked in one scan of the
middle. A union member of this shape costs what it would alone.

### 4.3 The literal set

The literal paths — a union of file names — are kept apart as one
sorted set of whole paths, searched once by binary search, up to
4,096 of them.

### 4.4 `match_dir` off a lone line

One line with at most one globstar answers `match_dir` by the same
anchored compares, reading off the line the tokens the NFA would hold
after the directory: a head position when the directory ends inside
the head, the globstar when it has taken everything after the head, a
tail position when the tail's first elements take the last segments.
Each tail alignment is its own run, so they are budgeted at 4,096
element compares (`MAX_TAIL_CHECKS`; tails of up to 90 elements stay
exact), past which the answer is the safe one. Such a matcher keeps no
NFA at all.

## 5. Picking a union's chains

Before the chains run, every call takes the suffix prefilter (§03).
Then the literal set is searched, and the chains are tried in pattern
order, by one of two pickers:

- **Gates**, for at most 10 wildcard chains (`LINEAR_CHAINS`), and for
  64 wildcard chains and the literal set (one chain more than a word
  has bits). Each chain is tried after two quick looks read off its
  ends: the path's last byte against a sketch of the bytes the chain
  may end with, its first byte against a literal head's; then the last
  segment against the chain's last literal or affix (`ends_like`).
- **The chain index**, for more. Each chain is filed under the literal
  strings one of its anchored segments must have: its last element's
  segment (a name, `.DS_Store`; the end of an extension, `*.log`; the
  start of a prefix, `._*`; a literal it holds, `*.so.*`, by its first
  16 bytes at most), else its first's, else the first of its block —
  read off that segment's nodes, small classes and braces expanded
  (`[Dd]ebug`, `*.py[cod]`), while the graph is built. The literal set
  is filed under its paths' first segments. A key is its length and
  outer four bytes, a lookup one hash probe, and a 64-bit sketch of the
  outer bytes skips the lookups that cannot hit. A path looks up its
  last segment, then its first, then the others, and walks each chain
  picked, without the gates; a chain no key files is tried on every
  call. The walk decides; the index only chooses which chains to ask,
  so a 600-member .gitignore costs a few lookups and one or two walks,
  not 60. The keys are held to one per unit of pattern weight, never
  fewer than 4,096: a segment whose keys pass what is left has none,
  and a union whose chains would file more entries than that is
  scanned with gates instead.

## 6. The graph NFA

The NFA over the graph has one state per node plus the accept state,
and steps once per path segment: a state whose element takes the
segment enters the closure precomputed for it (its successors, through
a nullable `**/` or a join to theirs; a globstar keeps itself). It
serves `match_dir` for every matcher but a lone line, and `is_match`
when the graph keeps no chains. Each ε-edge points to a later node, so
every table is built in one backward pass. Its representation is
picked by size:

| Kind     | When                                                                                             | Step                                          |
| -------- | ------------------------------------------------------------------------------------------------ | --------------------------------------------- |
| `Small`  | ≤ 64 states (32 in JS), no pieces                                                                | one `u64` active set, one OR per firing state |
| `Wide`   | Rust only: rows within 4 KiB (about 170 states) and no position leading to more than 48 literals | the same over word rows                       |
| `Sparse` | everything else, and always with pieces                                                          | per position its successors, literals sorted  |

The sparse NFA keeps, per position, its successors in three parts —
the literal ones sorted by literal, the nullable ones, the rest — and
its active set lists states and whole positions ("every successor of p
entered"), so a step costs what it tests (one binary search for a
position's literals) and the tables are linear in the graph.
Entering a join enters its successors, as a `G0` does without staying.

`match_dir`'s prefix bit is `active ∩ reach1`, where `reach1[s]` holds
iff some continuation of at least one segment from s reaches accept.
Its all-below bit steps the active set over wildcard-only segments
until it settles, then searches by length class (§04 §2.5). Both give
up past their caps with the safe answer (GLOB_SPEC §15.5). The fired
sets are the same in every kind, visited in one order, so every kind
and both runtimes answer alike.

## 7. In-segment matchers

Each closed segment with a wildcard compiles, in `compile_wild`, to the
first kind that holds it:

| Kind       | Shape                                                                                               | Match                                                           |
| ---------- | --------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `Affix`    | literal, a run of `*`/`?`, literal (or any part empty): `*.ts`, `a?b`                               | length bounds and two anchored compares                         |
| `Suffixes` | literal, a run of `*`/`?`, then an all-literal alternation: `*.{ts,tsx}`, a factored union of names | the prefix, then the suffixes that end as the segment does      |
| `Generic`  | anything else: classes, braces with wildcards, several runs                                         | a bit-parallel Thompson NFA over the segment's nodes (`SegNfa`) |

A segment holding an escaped `\` on Windows takes no path segment
(GLOB_SPEC §4.2): it is kept as an `Affix` that wants more bytes than
any segment has.

### 7.1 Suffix sets

The alternation is a list of any length, or a product of braces up to
1,024 suffixes whose bytes stay within 64 KiB or four times the nodes
they come from; past either the segment is `Generic`, which holds a
product without multiplying it out. The suffixes are grouped by their
last byte, so a segment is compared only with the few that end as it
does, in pattern order; four or fewer are scanned after a sketch of
their last bytes.

### 7.2 The in-segment NFA

One state per node: a literal byte, a class (a 256-bit set, negation
and separators applied), an any-byte state — a `?` moves on, a `*` is
its own successor with an ε-edge out — and the splits of a brace. The
active set is a `u64` up to 64 states (an int32 up to 32 in JS), a word
slice beyond. Every ε-edge points to a later state, so one ascending
sweep closes a set. Each state's closure is kept while all of them fit
128 KiB (about 1,000 states), and a step ORs the closure of each state
a byte fires; past that a step closes its set as it goes, so memory
stays linear.

Under `dot=false` a protected leading `.` is taken only by a literal
`.` or a positive class listing it: the entry closure of a dot-led
segment drops `*`, `?` and negated classes and passes no `*`. When no
state of that closure takes a `.`, the whole segment is
dot-protected: a dot-led segment is refused before the NFA runs.

### 7.3 What wildcards alone take

For `match_dir`'s all-below, each element knows the lengths whose
every segment its wildcards alone take (`Lens`: bits for 1 to 31, and
every length from some L ≤ 32 on). An affix or a suffix set reads them
off its shape; the in-segment NFA finds them in one pass over its
states in order, since every edge but a star's loop points to a later
state. `any_seg` is the case "every non-empty segment".

### 7.4 Glued pieces

One more NFA of the same states holds every glued piece of the graph
(`GlueNfa`): a piece's states, then a fan of ε-edges to the pieces
glued after it and an `Exit` where its segment may end. Nodes are laid
out in graph order and a piece is glued only to later ones, so every
ε-edge still points to a later state. The sparse graph NFA notes the
entry state of each piece a step holds, matches the segment from all
of them at once, and goes on from the nodes whose exits it reaches.
For `match_dir`, the same one pass over its states gives each exit the
lengths its wildcards alone take from the entries, so a cut pattern
proves all-below as the copied segments would.

## 8. Unions

`Glob::union` factors its members into one body before the walk
(`union.rs`): the whole tokens all members start or end with are
lifted out, then the bytes their first or last literals share (only
behind a lifted token, so the static prefixes stay). Members that
differ only in the literal ending one segment are merged into one
member with a brace there: `**/node_modules/**` and `**/.git/**`
become `**/{node_modules,.git}/**`, `**/*.log` and `**/*.tmp` become
`**/*{.log,.tmp}`, `src/**/*.ts` and `tests/**/*.ts` become
`{src,tests}/**/*.ts`. N chains each comparing a name become one
comparing a suffix set (§7.1). Merging may read 16 nodes per node of
the members, never fewer than 4,096; a union that would read more is
not merged.

## 9. Thresholds

Every threshold picks a representation; none rejects a pattern. The
only limits are the parser's: 64 KiB of pattern and 32 levels of brace
nesting.

| Threshold                         | Value                                     | Past it                                  |
| --------------------------------- | ----------------------------------------- | ---------------------------------------- |
| copy room (`graph.rs`)            | 1 per weight, ≥ 16,384                    | segments are cut into glued pieces       |
| wildcard chains (`MAX_CHAINS`)    | 64                                        | no chains: the NFA answers               |
| literal paths (`MAX_LITERALS`)    | 4,096, bytes ≤ max(64 KiB, 4·weight)      | no chains: the NFA answers               |
| gated chains (`LINEAR_CHAINS`)    | 10                                        | the chain index picks                    |
| index keys (`key_room`)           | 1 per weight, ≥ 4,096                     | a segment gives none; a union is scanned |
| tail compares (`MAX_TAIL_CHECKS`) | 4,096                                     | `chain_dir` gives the safe answer        |
| `Small` NFA                       | 64 states (JS 32)                         | `Wide` (Rust) or `Sparse`                |
| `Wide` NFA (Rust)                 | 4 KiB of rows, 48 literals per position   | `Sparse`                                 |
| suffix product                    | 1,024 suffixes, 64 KiB or 4·nodes         | `Generic`                                |
| suffix scan                       | 4 suffixes                                | grouped by last byte                     |
| kept closures                     | 128 KiB                                   | the step closes its set                  |
| in-segment `u64`                  | 64 states (JS 32)                         | a word slice                             |
| wildcard-only cover steps         | 64                                        | all-below: `false`                       |
| cover sets, firings               | 64 sets, 512 firings, 64 states per state | all-below: `false`                       |
| union merge reads                 | 16 per node, ≥ 4,096                      | the members stay apart                   |
| suffix prefilter set (`facts.rs`) | 16 branches                               | no prefilter                             |

## 10. What each layer buys

Each fast path above was measured by turning it off alone, at
`60e4f4e` (2026-10-24): Rust, functions and jump targets aligned as in
`scripts/bench.mjs`, the bench rows of BENCHMARKS.md, best of three
alternating runs (a run against itself moves rows by about 5%). The
ratios are off / on, on the rows the change moves most.

| Turned off                         | What runs instead                 | Cost                                                                                            |
| ---------------------------------- | --------------------------------- | ----------------------------------------------------------------------------------------------- |
| chains (§4)                        | the graph NFA for every call      | `is_match` 2.7× geomean, 6.1–13.5× on the big ignore lists; single-pattern `match_dir` 1.9–2.9× |
| blocks (§4.2)                      | the NFA for unions with `**/x/**` | `is_match` 2.2× (ignore-list-40) to 13.8× (global-586)                                          |
| the chain index (§5)               | gates for every union             | `is_match` 1.4× (ignore-list-40) to 7.4× (global-586) on every union of more than ten chains    |
| gates (§5)                         | the index for 2–10 chains too     | mixed-roots `is_match` 1.28×, memory 1.33×                                                      |
| `match_dir` off a lone line (§4.4) | an NFA for single patterns too    | `match_dir` 2.0–2.8×, memory 1.17–1.47×, build 1.04–1.27×                                       |
| dense graph NFAs (§6)              | `Sparse` for every graph          | union `match_dir` 1.3× (global-586) to 6.3× (mixed-roots)                                       |
| suffix sets (§7.1)                 | `Generic` for every alternation   | `*.{ts,tsx,js,jsx}` 2.0×; global-586 `is_match` 42×, memory 3.7×                                |
| kept closures (§7.2)               | the closing step                  | class-anychar `is_match` 1.8×                                                                   |

Every one of them pays for itself somewhere, by at least a quarter.

## 11. Complexity and safety

Compile is linear in the pattern: a brace-free pattern walks its nodes
once, a separator-crossing brace adds one node per branch rather than
a product, and what it copies is bounded by the room, past which it
cuts pieces. Every table built from the graph — chains, suffix
products, in-segment closures, NFA rows, index keys — has a size bound
past which a linear representation takes over (§9), so memory is
linear too. Matching is O(n·m) in the worst case, m the graph's nodes
and edges and the glued pieces' states, all linear in the pattern:
the NFA over the graph is the non-backtracking floor that rules out
ReDoS. A chain with at most one globstar is O(n).

`match_dir` adds at most 65 wildcard-only segments for the all-below
proof (64, and the step that shows the set has settled), then, where
elements split the names by length, at most 64 more (one per set of
states a length class fires; 512 firings, holding at most 64 states
per NFA state in all; a set with pieces costs one pass over the glued
pieces' states), and 4,096 element compares for a chain's tail tokens;
past them it gives the safe answer (GLOB_SPEC §15.5). The engine
answers `is_match`, `match_dir` and `static_prefixes` natively.

## 12. JS dual-mode execution

The JS port runs the same algorithm directly on JS strings
(`charCodeAt`/`startsWith`/`endsWith`/`indexOf` intrinsics, zero
per-call allocation). Byte-exactness is preserved by construction:
literal compares and `*`/globstar absorption are UTF-16-unit-count
independent, and the only counting constructs — `?` (one byte) and
negated classes (one byte) — read a non-ASCII segment's UTF-8 length,
or run on that one segment's bytes, so nothing is matched twice.
Patterns containing non-ASCII bytes compile to byte mode outright. The
string↔byte equivalence is fuzzed by
`packages/globstar/tests/string-mode.mjs`.

Where the twins differ, it is in representation only: JS bit sets are
int32 words, so its small graph NFA and small in-segment NFA hold 32
states; it has no `Wide` graph NFA (a graph past 32 states is sparse);
and it finds a suffix group, however few the suffixes, by a binary
search of their keys: a typed table costs V8 more to allocate (about
200 ns) than the search saves.

## 13. Source map

| Rust (`crates/globstar/src/`)   | JS (`packages/globstar/src/`)   | Role                                                |
| ------------------------------- | ------------------------------- | --------------------------------------------------- |
| `engine/segment/mod.rs`         | `engine/segment/index.js`       | the matcher: build, what a call runs (§2)           |
| `engine/segment/elem.rs`        | `engine/segment/elem.js`        | the elements (§1)                                   |
| `engine/segment/graph.rs`       | `engine/segment/graph.js`       | the graph walk (§3)                                 |
| `engine/segment/chain.rs`       | `engine/segment/chain.js`       | chains, gates, `match_dir` off a lone line (§4, §5) |
| `engine/segment/chain_index.rs` | `engine/segment/chain-index.js` | the chain index (§5)                                |
| `engine/segment/graph_nfa.rs`   | `engine/segment/graph-nfa.js`   | the graph NFA and the all-below search (§6)         |
| `engine/segment/wild.rs`        | `engine/segment/wild.js`        | affixes, suffix sets, `Lens` (§7)                   |
| `engine/segment/seg_nfa.rs`     | `engine/segment/seg-nfa.js`     | the in-segment NFA and the glue NFA (§7.2–7.4)      |
| `engine/segment/facts.rs`       | `engine/segment/facts.js`       | suffix prefilter (§03), static prefixes (§04)       |
| `engine/segment/bits.rs`        | `engine/segment/bits.js`        | bit sets; in Rust, the byte sketch too              |
| `union.rs`                      | `union.js`                      | factoring a union's members (§8)                    |
