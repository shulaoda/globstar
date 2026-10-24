# 03. The Segment Engine

Source: `crates/globstar/src/engine/segment/` (Rust) and `packages/globstar/src/engine/segment/` (JS). The JS implementation is a twin of the Rust one: the same layers in the same files, the same thresholds and the same answers. §12 lists the points where they differ.

## 1. The model

Every construct of the dialect denotes a union, a concatenation or a closure of byte sets (GLOB_SPEC §7–§10), so the language of a pattern is regular, and membership is decided by a nondeterministic automaton without backtracking. The engine builds that automaton over segments rather than bytes.

Every construct except `**` is confined to one segment (GLOB_SPEC §2.3): `*`, `?` and `[...]` cannot cross a separator, and dot protection is decided at segment starts. A `**` consumes whole segments: a leading `**/`, a middle `/**/`, a trailing `/**`, or the whole pattern. A pattern is therefore a graph of segment-shaped elements, and a path is matched one segment per element.

| Element        | Consumes                                                 | Source                                        |
| -------------- | -------------------------------------------------------- | --------------------------------------------- |
| `Lit(bytes)`   | one segment equal to the bytes                           | `src`, `.git`, an empty segment               |
| `Wild(m)`      | one segment the in-segment matcher `m` accepts (§7)      | `*.ts`, `[Dd]ebug`, `{a,b}?`                  |
| `G0`           | zero or more segments, empty ones included               | `**/`, `/**/`, a pattern that is `**`         |
| `G1`           | one or more segments                                     | a trailing `/**`: `a/**` accepts `a/`, not `a` |
| `Join`         | nothing; it stands for its successors                    | the graph walk, to share edges (§3.3)         |
| `Piece(entry)` | part of a segment, matched with the pieces glued to it   | the graph walk, past the copy room (§3.4)     |

Empty segments are first-class (`a//b` is the sequence `a`, ε, `b`), which makes the strict separator rule of GLOB_SPEC §7.4 a matter of counting elements. A `**/` consumes empty segments as well, which is what makes the separators around it lenient (§9.5 of the spec).

A pattern without a brace whose branches cross a separator is one path through the graph, a *line* of elements. A crossing brace makes the graph branch. Most patterns are lines, and most of the layers below exist so that lines, and unions of lines, never reach the general machinery.

## 2. The layers

Compilation is one pass per layer, each linear in the pattern:

1. **Parse** (note 01): the AST, a union's members factored into one body.
2. **Literal ends** (`literal_ends.rs`): the suffix prefilter (§13) and the static prefixes (§14), read from the AST as written.
3. **Graph walk** (`graph.rs`): the AST to the element graph (§3). Each closed segment is compiled to its element (`wild.rs`, §7); the segments a brace cuts into pieces become one `GlueNfa` (`seg_nfa.rs`, §7.5).
4. **Chains** (`chain.rs`): the paths through the graph, while they are few and simple (§4).
5. **Picker**: how a union tries its chains, each after its gate (`chain.rs`) or only those the chain index selects (`chain_index.rs`) (§5).
6. **Graph NFA** (`graph_nfa.rs`): built unless one line serves alone (§6).

What a call runs depends only on what compilation kept:

| The matcher keeps                    | `is_match`, after the prefilter                                                    | `match_dir`               |
| ------------------------------------ | ---------------------------------------------------------------------------------- | ------------------------- |
| one line (a chain with ≤ 1 globstar) | the line's anchored comparisons (§4.1)                                             | read from the line (§4.4) |
| chains, a picker and an NFA          | the literal set, then the chains: each after its gate, or those the index selects  | the NFA (§6)              |
| an NFA alone                         | the NFA                                                                            | the NFA                   |

The graph keeps no chains, and the NFA answers `is_match` as well, when a brace cut a segment into pieces, when there are more than 64 wildcard paths and their nodes exceed four times the pattern's weight or the chain index cannot select them (§5), when the literal paths take more bytes (one more per path) than 64 KiB and four times the pattern's weight, or when a path has more than two globstars, or two with nothing between them.

## 3. The graph

One walk over the AST builds the graph. It keeps one *group* per distinct pair (boundary, partial segment) that reaches a point of the pattern: the nodes awaiting a successor (the group's *tails*), the boundary just crossed, and the in-segment nodes read since. Groups that agree on all three continue as one, so their continuations are the same nodes.

### 3.1 Braces

A brace whose branches cross a separator is not multiplied out: each branch is walked from the groups before the brace, and the groups the branches leave merge again. `{a/,b/}x` is three nodes with `x` shared, and `{a/,b/}` repeated `k` times gives `2k + 1` nodes rather than `2^k` sequences. The in-segment branches of such a brace are walked as one choice (`{a,b,/}` as `{{a,b},/}`, `regroup`). A node is reused when its element and predecessors already exist, which turns a union of names into a trie. Two hash maps serve the walk: one finds a node to reuse (JS scans its first 16 nodes before keying them), one finds a group by its key past the first 16 groups.

A brace that crosses no separator stays inside its segment and belongs to the in-segment matcher (§7).

### 3.2 Globstars

The parser reads a `**` beside text as a star (note 01, §2.1). One beside a separator, a brace or an edge of the pattern is decided per group as the brace expansions reach it (GLOB_SPEC §9.1): a group at a boundary that reads `**` holds it until the next token; a `/` makes it a pending `**/`, the end of the pattern a bare `**`, and anything else a star. Pending `**/`s count once (`**/**/` ≡ `**/`), except that one whose group gathered tails from several branches becomes a `G0` of its own when the next token is read, so that `{**,a}/` repeated `k` times keeps about `5k` edges rather than `k²`. The empty expansions of a brace beside a `**` continue as a *shadow* group while the `**` may still be a globstar (`{a,}**`, `**{,a}/`), and end once text makes it a star.

### 3.3 Joins

A node takes an edge from each tail of its group, and a group is copied into every branch of a crossing brace. A group of `k` tails that enters a brace of `m` branches therefore first gathers its tails into one `Join` node when `k · m > k + m`, so that the branches share one edge instead of taking `k` each. Braces that may be empty, side by side, then keep a few edges per brace: `{a/,}` repeated `k` times is about `2k` nodes and `4k` edges, where each `a` would otherwise follow every one before it (`k²/2` edges). No NFA set holds a join, and a chain omits it.

### 3.4 The copy room and pieces

A segment that a brace's edge falls inside is copied into each branch: `x{a/,b}y` reads the segments `xa` and `xby`, and text after the brace is added to every segment the branches leave open. Partial segments of different lengths give a group each, so `a{a,/}` repeated `k` times would copy `O(k²)`. The copies are paid from a room of one unit per unit of pattern weight, and at least 16,384 units (`COPY_PER_WEIGHT`, `MIN_COPY_ROOM`), where a literal weighs its length, a class one plus its ranges, a brace one plus its branches (an empty branch weighs one) and any other token one.

Past the room, the walk cuts each open segment into a *piece* at the brace's edge, and the pieces after the brace are *glued* to it: `x`, then `a` and `b` glued to `x`, and `y` to `b`. A piece is never copied, so the walk makes every node, edge and token a bounded number of times, and the graph is linear in the pattern. Ordinary patterns and large unions never reach the room, so their segments stay whole, with their chains and literal sets; a graph with pieces keeps no chains, and its sparse NFA holds the glue (§7.5).

## 4. Chains

`Graph::chains` enumerates the paths through the graph, depth first in pattern order, joins omitted. A path of literals only joins the literal set; every other path becomes a chain whose shape is fixed by the positions of its globstars. Let `m` be a chain's elements and `n` the path's segments.

### 4.1 A line: at most one globstar

- **No globstar.** Positional: `n` must equal `m`, and element `i` takes segment `i`. `O(n)`.
- **One globstar at `g`.** The head `0..g` is anchored against the first `g` segments (a literal is compared in place, without a scan for the segment's end), the tail `g+1..m` against the last `m − g − 1` segments, right to left. The globstar takes what remains; its arity (`G1` requires one segment, the overlap) is arithmetic on byte positions, and the `dot = false` rule is one scan of the absorbed bytes for `(^|/)\.`. `O(n)`, without search or backtracking.

### 4.2 A block: two globstars around elements

A path with two globstars around a block (`**/x/**`, `src/**/{a,b}/**`) is walked directly as well: the head from the front, the tail from the back, and the block found among the segments between, moving right only, with the dot rule checked in one scan of the middle. A union member of this shape costs what it would cost alone.

### 4.3 The literal set

The literal paths, a union of file names, are kept apart as one sorted set of whole paths and searched once by binary search, however many there are; only their bytes are bounded.

### 4.4 `match_dir` from a lone line

One line with at most one globstar answers `match_dir` by the same anchored comparisons, reading from the line the tokens the NFA would hold after the directory: a head position when the directory ends inside the head, the globstar when it has taken everything after the head, and a tail position when the tail's first elements take the last segments. Each tail alignment is a run of its own, so the runs are budgeted at 4,096 element comparisons (`MAX_TAIL_CHECKS`; tails of up to 90 elements stay exact), past which the answer is the safe one. Such a matcher keeps no NFA.

## 5. Selecting a union's chains

Every call passes the suffix prefilter (§13) first. The literal set is then searched, and the chains are tried in pattern order by one of two pickers:

- **Gates**, for at most 10 wildcard chains (`LINEAR_CHAINS`), and for up to 64 when the index cannot file them. Each chain is tried after two tests read from its ends: the path's last byte against a sketch of the bytes the chain may end with, and its first byte against a literal head's; then the last segment against the chain's last literal or affix (`ends_like`).
- **The chain index**, for more. Each chain is filed under the literal strings one of its anchored segments must have: its last element's segment (a name, `.DS_Store`; the end of an extension, `*.log`; the start of a prefix, `._*`; a literal it holds, `*.so.*`, by its first 16 bytes), else its first element's, else the first of its block. The strings are read from the segment's nodes while the graph is built, with small classes and braces expanded (`[Dd]ebug`, `*.py[cod]`). The literal set is filed under its paths' first segments. A key is its length and its outer four bytes, a lookup one hash probe, and a 64-bit sketch of the outer bytes skips the lookups that cannot hit. A path looks up its last segment, then its first, then the others, and walks each chain selected, without the gates; a chain no key files is tried on every call. The walk decides; the index only chooses which chains to ask, so a 600-member ignore list costs a few lookups and one or two walks. The keys are bounded by one per unit of pattern weight, and at least 4,096: a segment whose keys exceed what is left has none, and a union whose chains would file more entries than that is scanned with gates instead.

  Up to 64 chains, a step's selection is a word of bits. Past 64 the index names each chain by its id and is the only picker: a step's selections are sorted and walked once each (JS: listed once per call by a stamp), the inner segments' together, and an exact key compares the whole segment by a 32-bit hash, so names that differ only inside (`n17.js`, `n27.js`) file apart. The index is built only while no key files more than 64 chains and at most 64 go unkeyed or are searched for a literal, so that no lookup walks more than a scan of 64 would; otherwise the NFA answers. A union of 50,000 members of ten shapes keeps 5,000 chains this way and answers in microseconds, where the NFA takes a tenth of a millisecond.

## 6. The graph NFA

The NFA over the graph has one state per node and an accepting state, and it steps once per path segment: a state whose element takes the segment enters the closure precomputed for it (its successors, through a nullable `**/` or a join to theirs; a globstar keeps itself). It serves `match_dir` for every matcher but a lone line, and `is_match` when the graph keeps no chains. Every ε-edge points to a later node, so every table is built in one backward pass. Its representation is chosen by size:

| Kind     | Condition                                                                                         | Step                                           |
| -------- | ------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| `Small`  | at most 64 states (32 in JS), no pieces                                                           | one `u64` active set, one OR per firing state  |
| `Wide`   | Rust only: rows within 4 KiB (about 170 states), no position leading to more than 48 literals     | the same over word rows                        |
| `Sparse` | everything else, and always with pieces                                                           | per position its successors, literals sorted   |

The sparse NFA keeps, per position, its successors in three parts, the literal ones sorted by literal, the nullable ones, the rest, and its active set lists states and whole positions ("every successor of `p` is entered"), so a step costs what it tests (one binary search for a position's literals) and the tables are linear in the graph. Entering a join enters its successors, as a `G0` does without staying.

The prefix bit of `match_dir` is `active ∩ reach1`, where `reach1[s]` holds when some continuation of at least one segment from `s` reaches accept. The all-below bit steps the active set over wildcard-only segments until it settles (§15.4), and gives up past its cap with the safe answer (GLOB_SPEC §15.5).

## 7. In-segment matchers

Each closed segment with a wildcard compiles, in `compile_wild`, to the first kind that holds it:

| Kind       | Shape                                                                                               | Match                                                           |
| ---------- | --------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `Affix`    | literal, a run of `*`/`?`, literal, any part possibly empty: `*.ts`, `a?b`                          | length bounds and two anchored comparisons                      |
| `Suffixes` | literal, a run of `*`/`?`, an all-literal alternation: `*.{ts,tsx}`, a factored union of names      | the prefix, then the suffixes that end as the segment does      |
| `Pieces`   | literals, `?`s and classes between `*`s, no brace: `a*b*c`, `n*d[k-m]e?txt`                         | the pieces the `*`s cut, each at its leftmost place             |
| `Generic`  | anything else: a brace around wildcards or classes, a product past its cap                          | a bit-parallel Thompson NFA over the segment's nodes (`SegNfa`) |

A segment that holds an escaped `\` on Windows takes no path segment (GLOB_SPEC §4.2): it is kept as an `Affix` that requires more bytes than any segment has.

### 7.1 Suffix sets

The alternation is a list of any length, or a product of braces of up to 1,024 suffixes whose bytes stay within 64 KiB or four times the nodes they come from; past either bound the segment is `Generic`, which holds a product without multiplying it out. The suffixes are grouped by their last byte, so that a segment is compared only with the few that end as it does, in pattern order; four or fewer are scanned in turn after a sketch of their last bytes.

### 7.2 Pieces

A brace-free segment is the pieces its `*`s cut, each of a fixed length, one byte, `?` or class per position (a class named once, however often it occurs). The first piece is compared at the segment's start and the last at its end, last position first, and each piece between is found at its leftmost place after the one before: that leaves the most room to the rest, so no match is missed and the search never moves back. Each piece is tried at each place in turn, so a segment costs `O(n · m)` at worst, as the in-segment NFA does. What the graph reads from a `Pieces` element is what it would read from the NFA: `NEVER` when a class takes no byte, and dot protection unless the segment starts with a literal `.` or a positive class that lists it.

### 7.3 The in-segment NFA

One state per node: a literal byte, a class (a 256-bit set with negation and separators applied), an any-byte state (a `?` moves on; a `*` is its own successor with an ε-edge out) and the splits of a brace. The active set is a `u64` up to 64 states (an int32 up to 32 in JS) and a word slice beyond. Every ε-edge points to a later state, so one ascending sweep closes a set. Each state's closure is kept while all of them fit in 128 KiB (about 1,000 states), and a step ORs the closure of each state a byte fires; past that a step closes its set as it goes, so memory stays linear.

Under `dot = false` a protected leading `.` is taken only by a literal `.` or a positive class that lists it: the entry closure of a dot-led segment drops `*`, `?` and negated classes and passes no `*`. When no state of that closure takes a `.`, the whole segment is dot-protected and a dot-led segment is refused before the NFA runs.

### 7.4 What wildcards alone take

For the all-below bit of `match_dir`, each element knows whether its wildcards alone take every non-empty segment (`any_seg`). An affix or a suffix set reads it from its shape (`*`, `?*`, a suffix that may be empty); pieces never have it, since a literal or a class stands in each; and the in-segment NFA finds it in one pass over its states in order, since every edge but a star's loop points to a later state: the pass computes the lengths its wildcards take, one bit for each length below 64 and every length from a tail on.

### 7.5 Glued pieces

One more NFA of the same states holds every glued piece of the graph (`GlueNfa`): a piece's states, then a fan of ε-edges to the pieces glued after it, and an `Exit` where its segment may end. Nodes are laid out in graph order and a piece is glued only to later ones, so every ε-edge still points to a later state. The sparse graph NFA notes the entry state of each piece a step holds, matches the segment from all of them at once, and continues from the nodes whose exits it reaches. For `match_dir`, the same pass over its states tells each exit whether the wildcards alone take every segment from the entries to it, so a cut pattern proves all-below as the copied segments would.

## 8. Unions

A union's members are factored into one body before the walk (note 01, §5): the tokens all members start or end with are lifted out of the brace, then the bytes their first or last literals share, and members that differ only in the literal ending one segment are merged into one member with a brace there. `N` chains that each compare a name thus become one that compares a suffix set (§7.1), and a union of file names becomes a trie of literal nodes and one literal set (§4.3).

## 9. Thresholds

Every threshold selects a representation; none rejects a pattern. The only limits are the parser's: 64 KiB of pattern and 32 levels of brace nesting.

| Threshold                                | Value                                     | Past it                                      |
| ---------------------------------------- | ----------------------------------------- | -------------------------------------------- |
| copy room (`graph.rs`)                   | 1 per unit of weight, at least 16,384     | segments are cut into glued pieces           |
| wildcard chains (`MAX_CHAINS`)           | 64; more while nodes ≤ 4 · weight         | no chains: the NFA answers                   |
| chains past 64, per index lookup         | 64 by one key, 64 unkeyed or searched     | no chains: the NFA answers                   |
| literal paths (`MAX_LITERAL_BYTES`)      | bytes + paths ≤ max(64 KiB, 4 · weight)   | no chains: the NFA answers                   |
| gated chains (`LINEAR_CHAINS`)           | 10                                        | the chain index selects                      |
| index keys (`key_room`)                  | 1 per unit of weight, at least 4,096      | a segment gives none; gates, past 64 the NFA |
| tail comparisons (`MAX_TAIL_CHECKS`)     | 4,096                                     | `chain_dir` gives the safe answer            |
| `Small` NFA                              | 64 states (JS 32)                         | `Wide` (Rust) or `Sparse`                    |
| `Wide` NFA (Rust)                        | 4 KiB of rows, 48 literals per position   | `Sparse`                                     |
| suffix product                           | 1,024 suffixes, 64 KiB or 4 · nodes       | `Generic`                                    |
| suffix scan                              | 4 suffixes                                | grouped by last byte                         |
| kept closures                            | 128 KiB                                   | the step closes its set                      |
| in-segment `u64`                         | 64 states (JS 32)                         | a word slice                                 |
| wildcard-only cover steps                | 64                                        | all-below: `false`                           |
| union merge reads                        | 16 per node, at least 4,096               | the members stay apart                       |
| suffix prefilter set (`literal_ends.rs`) | 16 branches                               | no prefilter                                 |

## 10. The cost of each layer

Each fast path was measured by disabling it alone, at `60e4f4e` (2026-10-24), the last two rows at `84fe3a5`: Rust, functions and jump targets aligned as in `scripts/bench.mjs`, the bench rows of BENCHMARKS.md, best of three alternating runs (a run against itself moves rows by about 5%). The ratios are off over on, on the rows the change moves most.

| Disabled                              | What runs instead                       | Cost                                                                                                                                       |
| ------------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| chains (§4)                           | the graph NFA for every call            | `is_match` 2.7× geometric mean, 6.1–13.5× on the large ignore lists; single-pattern `match_dir` 1.9–2.9×                                   |
| blocks (§4.2)                         | the NFA for unions with `**/x/**`       | `is_match` 2.2× (ignore-list-40) to 13.8× (global-586)                                                                                     |
| the chain index (§5)                  | gates for every union                   | `is_match` 1.4× (ignore-list-40) to 7.4× (global-586) on every union of more than ten chains                                               |
| gates (§5)                            | the index for 2–10 chains as well       | mixed-roots `is_match` 1.28×, memory 1.33×                                                                                                 |
| `match_dir` from a lone line (§4.4)   | an NFA for single patterns as well      | `match_dir` 2.0–2.8×, memory 1.17–1.47×, build 1.04–1.27×                                                                                  |
| dense graph NFAs (§6)                 | `Sparse` for every graph                | union `match_dir` 1.3× (global-586) to 6.3× (mixed-roots)                                                                                  |
| suffix sets (§7.1)                    | `Generic` for every alternation         | `*.{ts,tsx,js,jsx}` 2.0×; global-586 `is_match` 42×, memory 3.7×                                                                           |
| a word of bits up to 64 chains (§5)   | the id list past 64 for every index     | `is_match` 1.2–2.0× in Rust, 1.04–1.4× in JS, on every indexed union                                                                       |
| kept closures (§7.3)                  | the closing step                        | segments with a brace around wildcards (`{foo,bar}*`, `[ab]{c,d}*`): `match_dir` 1.04–1.3×, `is_match` 1.05–1.1×, for 10–18% less memory   |

Each layer costs at least a quarter of some bench row when disabled.

## 11. Complexity

Compilation is linear in the pattern: a brace-free pattern walks its nodes once, a crossing brace adds one node per branch rather than a product, and what it copies is bounded by the room, past which it cuts pieces. Every table built from the graph (chains, suffix products, in-segment closures, NFA rows, index keys) has a size bound past which a linear representation takes over (§9), so memory is linear as well. Matching is `O(n · m)` in the worst case, with `m` the graph's nodes and edges and the glued pieces' states, all linear in the pattern: the NFA over the graph is the non-backtracking floor that rules out exponential behavior. A chain with at most one globstar is `O(n)`.

`match_dir` adds at most 65 wildcard-only segments for the all-below proof (64, and the step that shows the set has settled; a set with pieces costs one pass over the glued pieces' states), and 4,096 element comparisons for a chain's tail tokens; past them it gives the safe answer (GLOB_SPEC §15.5). The engine answers `is_match`, `match_dir` and `static_prefixes` without any further construction.

## 12. Execution in JS

The JS implementation runs the same algorithm on JS strings (`charCodeAt`, `startsWith`, `endsWith`, `indexOf`), with no allocation per call. Byte exactness holds by construction: literal comparisons and the absorption of `*` and `**` do not depend on the count of UTF-16 units, and the only counting constructs, `?` (one byte) and negated classes (one byte), read a non-ASCII segment's UTF-8 length or run on that segment's bytes, so nothing is matched twice. A pattern with a non-ASCII byte compiles to byte mode outright. The equivalence of the string and byte modes is fuzzed by `packages/globstar/tests/string-mode.mjs`.

Where the twins differ, they differ in representation only: JS bit sets are int32 words, so its small graph NFA and small in-segment NFA hold 32 states; it has no `Wide` graph NFA (a graph past 32 states is sparse); and it finds a suffix group, however few the suffixes, by a binary search of their keys, since a typed table costs V8 more to allocate (about 200 ns) than the search saves.

## 13. The suffix prefilter

A *prefilter* for a language `L` is a predicate `A` with `A(w) = 0 ⇒ w ∉ L`; it may accept a `w ∉ L`, and it is cheaper than membership in `L`. The engine's prefilter is a literal suffix every match carries, compared at the end of the path before anything else runs.

### 13.1 Extraction

`literal_ends.rs::trailing` reads the pattern's nodes right to left: a literal contributes its bytes, a separator a `/` unless it is the `/` of a `**/`, and any other node stops the reading. The bytes, reversed, are the suffix; stopping at the first other node is required, since a wildcard or a `**` can produce any bytes. The `/` after a `**` belongs to that `**/`, which absorbs zero segments as well, so the suffix of `a/**/b` is `b` and `**/b` keeps matching `b`.

When the suffix is ε and the pattern ends with a brace of at most 16 branches (`MAX_SUFFIX_SET`), `suffix_set` derives one suffix per branch: the suffix of the nodes before the brace, appended to the branch's own suffix when the branch is all literal, and the branch's own suffix alone otherwise; a branch that gives ε gives up the set. `**/*.{ts,tsx,js}` has the set `{".ts", ".tsx", ".js"}`. A brace of more than 16 branches gets no prefilter; its literal branches are grouped by the suffix set of §7.1.

### 13.2 Comparison

`bytes::ends_with_pattern` compares the suffix with the end of the path, last byte first, under the byte rule of the literal engine (note 02, §2): a suffix `/` matches any byte of `Seps`, a separator byte in the suffix matches nothing, and letters fold under `case_insensitive`. Without the first rule, `*/main.ts` would reject `src\main.ts` on Windows.

### 13.3 Soundness

**Proposition 1.** For every pattern `P` and `w ∈ Σ*`, if the prefilter rejects `w` then `w ∉ L(P)`.

*Proof.* Both extraction procedures yield a set `S` such that every `w ∈ L(P)` ends with some `s ∈ S` under the byte rule. The prefilter accepts `w` exactly when `w` ends with some element of `S`. If it rejects `w`, then `w` ends with no element of `S`, so `w ∉ L(P)`. ∎

The prefilter is not applied to `match_dir`: a directory path does not in general end with the pattern's suffix, which lies in the file segment, so the test would reject directories with matches below them.

### 13.4 Suffix rather than prefix

The engine reads from the left, so a prefix mismatch is found at the first differing segment or byte, and a prefix test would repeat that work. A suffix mismatch is found last: the NFA sees the end last, and a chain finds its tail after its head. An end-anchored test at the matcher's entry rejects a mismatched tail in `O(|suffix|)` without entering the engine. This is the common case on a walk, where most paths share the pattern's literal head and differ in the extension.

## 14. Static prefixes

### 14.1 Definition

For a pattern `P` with brace expansions `E(P)` (GLOB_SPEC §10.1), let `head(e)` be the longest literal prefix of the expansion `e` that ends at a segment boundary: every `w ∈ L(e)` starts with it, and either `head(e) = ε`, or every such `w` equals `head(e)` or continues after it with a separator. The static prefixes of `P` are the minimal elements of `{ head(e) : e ∈ E(P) }` under "is a segment-prefix of", an antichain. The separator after a head is not kept, a fully literal pattern is its own prefix, and the expansions of a union are its members'. The heads are bytes in the pattern's spelling: `/` for every separator, and the pattern's case.

### 14.2 Extraction

`literal_ends.rs::static_prefixes` reads the AST left to right, collecting bytes from literals and `/` from separators. A brace in the head is read branch by branch, each followed by what follows the brace, as a continuation rather than a copy. At the first other node (a wildcard, a `**`) the bytes are cut back to the last separator before it; a brace that the budget or the depth limit stops, and whose branches all start with a separator, ends a whole segment instead. Trailing separators are dropped. Each path through the braces of the head records one head.

Re-reading the continuation once per branch is the only part that can cost more than the pattern: `{a,b}{c,d}…` doubles per brace, and a brace before a long literal re-reads it once per branch. Each byte read and each branch taken therefore costs one unit of a budget of `4096 + 2 · weight(P)`, and braces nest at most 64 deep along one head; past either, the head ends at its last whole segment, a shorter prefix that is still sound. Recording a head costs every byte read to reach it, and these could still sum to the square of the budget; past 64 budgets of such bytes the reading starts again as if at the depth limit, and the result is the one head before the first brace, which every expansion shares.

With several heads, `dedupe` sorts them by length and bytes and drops every head a shorter kept one covers (`src` covers `src/a`); the empty head covers all.

```
src/main.rs        →  { "src/main.rs" }
src/*.ts           →  { "src" }
**/*.ts            →  { "" }
{src,test}/*.rs    →  { "src", "test" }
{src,src/a}/*.rs   →  { "src" }
{a/**,b}/c         →  { "a", "b/c" }
a{b,c}d/x          →  { "abd/x", "acd/x" }
```

### 14.3 Soundness

**Proposition 2.** For every `w ∈ L(P)` there is a static prefix `s` such that `w` starts with `s` at a segment boundary.

*Proof.* Every `w ∈ L(P)` lies in `L(e)` for some expansion `e`, and the reading follows the branches of `e` through the braces of the head. The bytes it keeps come only from literal and separator nodes before the first other node, cut back to a segment boundary, so every `w ∈ L(e)` starts with them. A head cut short by the budget is a segment-prefix of `head(e)` and starts every such `w` as well, and `dedupe` drops a head only when a shorter kept head covers it. ∎

## 15. The directory predicate

### 15.1 Definition

GLOB_SPEC §15.2 and §15.5 define, for a pattern `P` and a directory `d`, the three facts `exact(P, d) = d ∈ L(P)`, `prefix(P, d) = below(d) ∩ L(P) ≠ ∅` and `all_below(P, d)`, and the result as their combination:

```
combine(true,  _,     true)  = DescendAllAndMatch
combine(false, _,     true)  = DescendAll
combine(true,  true,  false) = DescendAndMatch
combine(true,  false, false) = Match
combine(false, true,  false) = Descend
combine(false, false, false) = Pruned
```

`prefix` is over-approximated and `all_below` under-approximated, each on the consumer's safe side. At the root the engine reports `prefix` as true for every pattern. `Pruned` is the only result that lets a walker skip the directory; the two `DescendAll…` results let a consumer that excludes files by pattern skip the directory whole.

### 15.2 Realization

A matcher with one chain and at most one globstar answers from the chain (§4.4). Every other matcher splits `d` at separators and runs its NFA over the graph (§6); the active set is then classified with the accepting bit and the precomputed `reach1` mask:

```
S       ← the state set after the segments of d
exact   ← S holds an accepting state
prefix  ← d = "" or S intersects reach1
all     ← dot and prefix and covers_below(S)
result  ← combine(exact, prefix, all)
```

The chain path and the one-word NFA allocate nothing per call. Past that, Rust allocates the NFA's sets per call and JS keeps them on the matcher.

### 15.3 Negated patterns

A negated pattern returns `DescendAndMatch` when the directory matches and `Descend` otherwise (GLOB_SPEC §15.4): its language is the complement of another, and the deeper paths, unseen, cannot be excluded from that complement. The JS matcher accepts negated union members (note 01, §5.1); with one present, `exact` is the union's own answer, `prefix` is true, and the positive members alone decide `all_below`, so no directory is pruned. The walkers route `!`-prefixed patterns into their ignore sets (note 04, §2), so their matchers hold positive patterns only.

### 15.4 All-below

The engine decides `all_below` by simulation. A *wildcard-only* byte is one that `?`, `*` and `**` consume and that every literal and class rejects; a wildcard-only path is built from such bytes and separators. From the state set that `d` leaves, the simulation feeds wildcard-only segments and requires an accepting state after each:

```
S ← the state set after the segments of d
repeat
    S ← step S over a wildcard-only segment
        (a globstar absorbs it; a Wild consumes it iff its wildcards alone
         accept every non-empty segment; a Lit never does)
    if S holds no accepting state:  return false
    if S did not change:            return true
```

Tokens move right along a sequence or stay on a globstar, so the loop ends within the number of states; it is capped at 65 steps (GLOB_SPEC §15.5). The bit per `Wild` is the `any_seg` of §7.4. A segment that a brace cut into glued pieces (§3.4) is no element of its own: the pieces a set holds enter the glue NFA at once, and the pass of §7.5 tells each exit whether the wildcards alone accept every name there; the loop fires those nodes, and the answer is the one the copied segments would give. On a lone chain the simulation has a closed form (§4.4): the globstar is alive after `d`, every tail element has the bit, and every shorter tail token is alive as well, within the budget of 4,096 element comparisons.

Under `dot = false` the answer is `false` without simulation: `**` does not match a dot-led name, so some `d/.x` escapes.

One case is not provable: branches that split the names by length, `?/**/?` with `**/??*`, accept every name together, but the rule asks each `Wild` to accept every length on its own and answers `false`. Union members that merge into one segment (`**/?` with `**/??*` factors to `**/?{,?*}`) are one `Wild` and are proven; factoring merges members and not the braces a pattern writes, so `{**/?,**/??*}` keeps two branches and is not.

### 15.5 Soundness

**Proposition 3 (no false pruning).** For every `P`, `d` and `w ∈ Σ*`, `match_dir(P, d) = Pruned` implies `d ∉ L(P)` and `d · "/" · w ∉ L(P)`.

*Proof.* `Pruned` is returned only when `exact` and `prefix` are both false. `exact = false` excludes `d`. `prefix = false` means that no active state can consume a separator and a continuation, possibly empty, to an accepting state. Neither `d` nor any path below it is accepted. ∎

**Proposition 4 (no false cover).** For every `P`, `d` and `s ∈ R`, `match_dir(P, d) ∈ { DescendAll, DescendAllAndMatch }` implies `d · "/" · s ∈ L(P)`.

*Proof.* Take any `s ∈ R` and the wildcard-only path `s'` of the same shape: the same number of segments, of the same lengths. Every transition a wildcard-only byte fires, a non-separator byte of `s` fires as well (`dot = true`, so no wildcard is dot-protected), and separators fire the same transitions in both. By induction the state set after `s` contains the state set after `s'`. The simulation visited `s'`, since it covers every shape, and found an accepting state there. ∎

Proposition 3 is what a walker relies on when it skips a directory; Proposition 4 is what a consumer that excludes files relies on when it skips one whole.

## 16. Source map

| Rust (`crates/globstar/src/`)    | JS (`packages/globstar/src/`)    | Role                                                         |
| -------------------------------- | -------------------------------- | ------------------------------------------------------------ |
| `engine/segment/mod.rs`          | `engine/segment/index.js`        | the matcher: build, what a call runs (§2), the prefilter test (§13), `match_dir` (§15) |
| `engine/segment/graph.rs`        | `engine/segment/graph.js`        | the elements (§1), the graph walk (§3)                       |
| `engine/segment/chain.rs`        | `engine/segment/chain.js`        | chains, gates, `match_dir` from a lone line (§4, §5)         |
| `engine/segment/chain_index.rs`  | `engine/segment/chain-index.js`  | the chain index (§5)                                         |
| `engine/segment/graph_nfa.rs`    | `engine/segment/graph-nfa.js`    | the graph NFA and the all-below search (§6, §15.4)           |
| `engine/segment/wild.rs`         | `engine/segment/wild.js`         | affixes, suffix sets, pieces (§7)                            |
| `engine/segment/seg_nfa.rs`      | `engine/segment/seg-nfa.js`      | the in-segment NFA and the glue NFA (§7.3–§7.5)              |
| `engine/segment/literal_ends.rs` | `engine/segment/literal-ends.js` | the suffix prefilter (§13), the static prefixes (§14)        |
| `engine/segment/bits.rs`         | `engine/segment/bits.js`         | bit sets, byte sets of classes; in Rust, the byte sketch     |
| `dir_match.rs`                   | `dir-match.js`                   | the six results and their three facts (§15.1)                |
