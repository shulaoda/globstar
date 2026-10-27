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

Compilation is one pass per layer, each linear in the pattern; a layer that enumerates pays for it from a room of its own (§9):

1. **Parse** (note 01): the AST, a union's members factored into one body.
2. **Graph walk** (`graph.rs`): the AST to the element graph (§3). Each closed segment is compiled to its element (`wild.rs`, §7); the segments a brace cuts into pieces become one `GlueNfa` (`wild.rs`, §7.5).
3. **Literal ends** (`mod.rs`): the suffix prefilter (§13) and the static prefixes (§14), read off the graph.
4. **Chains** (`chain.rs`): the paths through the graph, while they are few and simple (§4).
5. **Picker**: how a union tries its chains, each after its gate (`chain.rs`) or only those the chain index selects (`chain.rs`) (§5).
6. **Graph NFA** (`graph_nfa.rs`): built unless one line serves alone (§6).

What a call runs depends only on what compilation kept:

| The matcher keeps                    | `is_match`, after the prefilter                                                    | `match_dir`               |
| ------------------------------------ | ---------------------------------------------------------------------------------- | ------------------------- |
| one line (a chain with ≤ 1 globstar) | the line's anchored comparisons (§4.1)                                             | read from the line (§4.4) |
| the literal set alone                | the set, by binary search (§4.3)                                                   | read from the set (§4.3)  |
| chains, a picker and an NFA          | the literal set, then the chains: each after its gate, or those the index selects  | the NFA (§6)              |
| an NFA alone                         | the NFA                                                                            | the NFA                   |

The graph keeps no chains, and the NFA answers `is_match` as well, when a brace cut a segment into pieces, when the wildcard paths weigh more than the room (§9, §11.1), when there are more than 64 of them and the chain index cannot select them (§5), when the literal paths take more bytes (one more per path) than the room, or when a path has more than two globstars, or two with nothing between them.

## 3. The graph

One walk over the AST builds the graph. It keeps one *group* per distinct pair (boundary, partial segment) that reaches a point of the pattern: the nodes awaiting a successor (the group's *tails*), the boundary just crossed, and the in-segment nodes read since. Groups that agree on all three continue as one, so their continuations are the same nodes.

### 3.1 Braces

A brace whose branches cross a separator is not multiplied out: each branch is walked from the groups before the brace, and the groups the branches leave merge again. `{a/,b/}x` is three nodes with `x` shared, and `{a/,b/}` repeated `k` times gives `2k + 1` nodes rather than `2^k` sequences. The in-segment branches of such a brace are walked as one choice (`{a,b,/}` as `{{a,b},/}`, `regroup`). A node is reused when its element and predecessors already exist, which turns a union of names into a trie. Two hash maps serve the walk: one finds a node to reuse, one finds a group by its key past the first 16 groups.

A brace that crosses no separator stays inside its segment and belongs to the in-segment matcher (§7), unless it is the whole pattern: a union's members, or a pattern written as one brace, are walked as branches whatever they hold, so that the names among them join the literal set and the rest are chains of their own, each with its keys (§5), rather than one segment's choice.

### 3.2 Globstars

The parser reads a `**` beside text as a star (note 01, §2.1). One beside a separator, a brace or an edge of the pattern is decided per group as the brace expansions reach it (GLOB_SPEC §9.1): a group at a boundary that reads `**` holds it until the next token; a `/` makes it a pending `**/`, the end of the pattern a bare `**`, and anything else a star. Pending `**/`s count once (`**/**/` ≡ `**/`). The empty expansions of a brace beside a `**` continue as a *shadow* group while the `**` may still be a globstar (`{a,}**`, `**{,a}/`), and end once text makes it a star.

### 3.3 Joins

Every node but a join has one predecessor. A group whose tails are several gathers them into one `Join` node before a node follows them or a crossing brace copies them into its branches; the join takes an edge from each tail, and what follows takes one from the join. Braces that may be empty, side by side, thus keep a few edges per brace: in `{a/,}` or `{**,a}/` repeated `k` times each node would otherwise follow every one before it (`k²/2` edges). And each element has one position before it, which an NFA step enters once (§6). No NFA set holds a join, and a chain omits it.

### 3.4 The copy room and pieces

A segment that a brace's edge falls inside is copied into each branch: `x{a/,b}y` reads the segments `xa` and `xby`, and text after the brace is added to every segment the branches leave open. Partial segments of different lengths give a group each, so `a{a,/}` repeated `k` times would copy `O(k²)`. The copies are paid from the room (§9), four units per unit of the weight copied (`COPY_COST`), since every copy compiles its tokens again; a literal weighs its length, a class one plus its ranges, a brace one plus its branches (an empty branch weighs one) and any other token one.

Past the room, the walk cuts each open segment into a *piece* at the brace's edge, and the pieces after the brace are *glued* to it: `x`, then `a` and `b` glued to `x`, and `y` to `b`. A piece is never copied, so the walk makes every node, edge and token a bounded number of times, and the graph is linear in the pattern. Ordinary patterns and large unions never reach the room, so their segments stay whole, with their chains and literal sets; a graph with pieces keeps no chains, and its sparse NFA holds the glue (§7.5).

## 4. Chains

`Graph::chains` enumerates the paths through the graph, depth first in pattern order, joins omitted. A path of literals only joins the literal set; every other path becomes a chain whose shape is fixed by the positions of its globstars. Let `m` be a chain's elements and `n` the path's segments.

### 4.1 A line: at most one globstar

- **No globstar.** Positional: `n` must equal `m`, and element `i` takes segment `i`. `O(n)`.
- **One globstar at `g`.** The head `0..g` is anchored against the first `g` segments (a literal is compared in place, without a scan for the segment's end), the tail `g+1..m` against the last `m − g − 1` segments, right to left. The globstar takes what remains; its arity (`G1` requires one segment, the overlap) is arithmetic on byte positions, and the `dot = false` rule is one scan of the absorbed bytes for `(^|/)\.`. `O(n)`, without search or backtracking.

### 4.2 A block: two globstars around elements

A path with two globstars around a block (`**/x/**`, `src/**/{a,b}/**`) is walked directly as well: the head from the front, the tail from the back, and the block found among the segments between, moving right only, with the dot rule checked in one scan of the middle. A union member of this shape costs what it would cost alone.

### 4.3 The literal set

The literal paths, a union of file names, are kept apart as one sorted set of whole paths and searched once by binary search, however many there are; only their bytes are bounded. A union whose paths are all literal keeps the set alone, no elements and no NFA, and answers `match_dir` from it too: `d` matches when the set holds it, something below may match when a path starts with `d/` (one more binary search), and nothing covers all below.

### 4.4 `match_dir` from a lone line

One line with at most one globstar answers `match_dir` by the same anchored comparisons, reading from the line the tokens the NFA would hold after the directory: a head position when the directory ends inside the head, the globstar when it has taken everything after the head, and a tail position when the tail's first elements take the last segments. Each tail alignment is a run of its own, so the runs are budgeted at 4,096 element comparisons (`MAX_TAIL_CHECKS`; tails of up to 90 elements stay exact), past which the answer is the safe one. Such a matcher keeps no NFA.

## 5. Selecting a union's chains

Every call passes the suffix prefilter (§13) first. The literal set is then searched, and the chains are tried in pattern order by one of two pickers:

- **Gates**, for at most 10 wildcard chains (`LINEAR_CHAINS`), and for up to 64 when the index cannot file them. Each chain is tried after two tests read from its ends: the path's last byte against a sketch of the bytes the chain may end with, and its first byte against a literal head's; then the last segment against the chain's last literal or affix (`ends_like`).
- **The chain index**, for more. Each chain is filed under the literal strings one of its anchored segments must have: its last element's segment (a name, `.DS_Store`; the end of an extension, `*.log`; the start of a prefix, `._*`; a literal it holds, `*.so.*`, by its first 16 bytes), else its first element's, else the first of its block. The strings are read from the segment's compiled element: an affix's prefix or suffix, a suffix set's suffixes, the end or start of a piece-wise segment with its small classes expanded (`[Dd]ebug`, `*.py[cod]`); a brace around wildcards gives none. The literal set is filed under its paths' first segments. A key is its length and its outer four bytes, a lookup one hash probe, and a 64-bit sketch of the outer bytes skips the lookups that cannot hit. A path looks up its last segment, then its first, then the others, and walks each chain selected, without the gates; a chain no key files is tried on every call. The walk decides; the index only chooses which chains to ask, so a 600-member ignore list costs a few lookups and one or two walks. A class product is cut at 64 strings, and the entries are bounded by the room (§9): a union whose chains would file more is scanned with gates instead.

  Up to 64 chains, a step's selection is a word of bits. Past 64 the index names each chain by its id and is the only picker: a step's selections are sorted and walked once each (JS: listed once per call by a stamp), the inner segments' together, and an exact key compares the whole segment by a 32-bit hash, so names that differ only inside (`n17.js`, `n27.js`) file apart. The index is built only while no key files more than 64 chains and at most 64 go unkeyed or are searched for a literal, so that no lookup walks more than a scan of 64 would; otherwise the NFA answers. A union of 50,000 members of ten shapes keeps 5,000 chains this way and answers in microseconds, where the NFA takes a tenth of a millisecond. Chains that share an element read its strings once.

## 6. The graph NFA

The NFA over the graph has one state per node and an accepting state, and it steps once per path segment: a state whose element takes the segment enters the closure precomputed for it (its successors, through a nullable `**/` or a join to theirs; a globstar keeps itself). It serves `match_dir` for every matcher but a lone line, and `is_match` when the graph keeps no chains. Every ε-edge points to a later node, so every table is built in one backward pass. Its representation is chosen by size:

| Kind     | Condition                                                                                         | Step                                           |
| -------- | ------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| `Small`  | at most 64 states (32 in JS), no pieces                                                           | one `u64` active set, one OR per firing state  |
| `Wide`   | Rust only: rows within 4 KiB (about 170 states), no position leading to more than 48 literals     | the same over word rows                        |
| `Sparse` | everything else, and always with pieces                                                           | per position its successors, literals sorted   |

The sparse NFA keeps, per position, its successors in three parts, the literal ones sorted by literal, the nullable ones, the rest, and its active set lists states and whole positions ("every successor of `p` is entered"), so a step costs what it tests (one binary search for a position's literals) and the tables are linear in the graph. Entering a join enters its successors, as a `G0` does without staying. A step enters each position once, and each element has one position before it (§3.3), so a step tests each element at most once.

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

The alternation is a list of any length, or a product of braces of up to 1,024 suffixes whose bytes stay within four times the nodes they come from, the rest drawn from the room (§9); past either bound the segment is `Generic`, which holds a product without multiplying it out. The suffixes are grouped by their last byte, so that a segment is compared only with the few that end as it does, in pattern order; four or fewer are scanned in turn after a sketch of their last bytes.

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

A union's members are factored into one body before the walk (note 01, §5): members that differ only in the literal ending one segment are merged into one member with a brace there, and the rest are the branches of one brace, walked as paths of their own whether or not they cross a separator (§3.1). `N` chains that each compare a name thus become one that compares a suffix set (§7.1), and a union of file names becomes a trie of literal nodes and one literal set (§4.3).

## 9. Thresholds

Every threshold selects a representation; none rejects a pattern. The only limits are the parser's: 64 KiB of pattern and 32 levels of brace nesting.

What a layer enumerates is paid from a *room* of its own, of `max(4W, 64 KiB)` units for a pattern of weight `W` (`graph.rs::room`): the copies of a cut segment (§3.4), the suffix bytes of a product past four times its nodes (§7.1), the bytes and nodes of the static prefixes (§14.2), the weight of the wildcard paths (§11.1), the bytes of the literal paths (§4.3) and the index's entries (§5). Each pays before it builds, and what its room cannot pay for keeps the pattern's form unexpanded.

| Threshold                                | Value                                     | Past it                                      |
| ---------------------------------------- | ----------------------------------------- | -------------------------------------------- |
| room (`room`), per kind of expansion     | max(4 · weight, 64 KiB)                   | that expansion stops                         |
| wildcard chains (`MAX_CHAINS`)           | 64                                        | only the chain index picks them              |
| chains past 64, per index lookup         | 64 by one key, 64 unkeyed or searched     | no chains: the NFA answers                   |
| gated chains (`LINEAR_CHAINS`)           | 10                                        | the chain index selects                      |
| tail comparisons (`MAX_TAIL_CHECKS`)     | 4,096                                     | `chain_dir` gives the safe answer            |
| `Small` NFA                              | 64 states (JS 32)                         | `Wide` (Rust) or `Sparse`                    |
| `Wide` NFA (Rust)                        | 4 KiB of rows, 48 literals per position   | `Sparse`                                     |
| suffix product                           | 1,024 suffixes, 4 · nodes + room          | `Generic`                                    |
| suffix scan                              | 4 suffixes                                | grouped by last byte                         |
| kept closures                            | 128 KiB                                   | the step closes its set                      |
| in-segment `u64`                         | 64 states (JS 32)                         | a word slice                                 |
| wildcard-only cover steps                | 64                                        | all-below: `false`                           |
| suffix prefilter set (`MAX_SUFFIX_SET`)  | 16 suffixes                               | one suffix, what they all share              |

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

### 11.1 Measures

For a pattern `P`, `|P|` is its length in bytes and `W` its weight (§3.4). A literal weighs its length, a class at most its bytes, a brace at most its bytes with the commas and the braces, any other token one, so `W ≤ |P|`. For a union of members `P_1, …, P_N`, `|P|` is `Σ |P_i| + N`, and the factored body weighs at most `|P| + 1`, since a merge leaves one member no heavier than the ones it replaces. A path `w` has `n` bytes and at most `n + 1` segments.

The *weight* `ω(e)` of an element bounds what testing it against a segment `u` costs per byte of `u`: one for a literal, an affix, a globstar, a join and a piece (whose test the glue NFA makes); the number of suffixes for a suffix set; the number of positions for pieces; the number of states for an in-segment NFA. A test costs `O((|u| + 1) · ω(e))`: a literal or an affix compares at most `|u|` bytes; a suffix set compares its prefix and then each suffix of the group `u`'s last byte selects, each in at most `|u|` bytes; pieces place each piece at most `|u|` times; an in-segment NFA steps each byte over its states, and the closures it keeps fit 128 KiB, which leaves at most 16 words a state. An element is compiled from one closed segment and weighs `O` of that segment's weight, except a suffix set, whose suffixes take at most four times the segment's weight and a share of the room (§7.1); so the elements of the graph, `ω(G)` together, weigh `O(W)` by Lemma 3.

The bounds count a hash-table operation as `O(1)`, which holds in expectation for keys an adversary does not choose; §11.4 names the tables whose hashes are not keyed.

### 11.2 Compilation

**Theorem 1.** Compiling `P` takes `O(|P| + W log W)` time and `O(|P| + W)` memory.

The theorem is the sum of the lemmas below, one per layer of §2.

**Lemma 1 (parse and factoring).** Parsing takes `O(|P|)` and leaves `O(|P|)` nodes. Factoring a union of total length `L` takes `O(L log L)`.

*Proof.* The parser reads each byte once; a brace of one branch hands its bytes to the enclosing literal (note 01, §2), at most once per level of nesting, so at most 33 times. Factoring hashes each member once, files at most one record per node and sorts the records. A group compares each free member with its head, and each comparison retires the member compared, absorbed or not (note 01, §5.3), so a member is compared at most once, at the cost of its length. ∎

**Lemma 2 (literal ends).** The suffix prefilter takes `O(W)`. The static prefixes take `O(W log W)` time and `O(W)` memory.

*Proof.* The suffixes take one pass over the edges for the predecessors and the ends, and each end's literal ends, of `O` its element's weight. An end that takes whole literal segments reads back one predecessor at a time, and none reads back once the set holds more than 16 suffixes, so at most 16 read-backs of at most every node are made. The head reader of §14.2 pays from its room, before it builds, every byte the heads take at a node and one unit per node it moves to, and records a head at most once per step it took: the heads hold `O(W)` bytes. `dedupe` sorts them, `O(C log C)` for `C` bytes, and walks the chain of kept prefixes: a failed test removes the chain's top for good, at the cost of its length, and the one test that does not fail costs at most the current head's length. ∎

**Lemma 3 (graph).** The walk takes `O(W)` time and builds a graph of `O(W)` nodes and edges whose elements weigh `O(W)`.

*Proof sketch.* Each node of a walked sequence is visited once, and a crossing brace's subtree is read by `crosses` once per enclosing level, at most 33 times. A visit costs the groups it reaches. After a separator the groups merge into at most four (§3.2: one for the closed segments, one per boundary of a pending `**/`), and a group whose segment is empty differs from another only in its boundary and three flags, so at most 24 such groups exist at once. Groups with text multiply only where a crossing brace copies them into its branches, and the copy is paid from the room (§9) before it is made; past the room the open segments become pieces, which are never copied (§3.4), and every later token added to `c` groups pays `c − 1` units of its weight. Several tails pass through one join before a node follows them or a brace copies them (§3.3), so a node takes one edge, a join one per tail it gathers, and a brace copies one tail per group into each branch. The walk thus makes `O(W)` nodes, edges and copies of tokens, and each closed segment compiles in time linear in its weight: suffix products are bounded before they are built (§7.1), the closures an NFA keeps by 128 KiB (§7.3), the glue NFA by its pieces and their edges (§7.5). ∎

**Lemma 4 (paths).** Enumerating the paths takes `O(W)`. The chains kept weigh at most the room, `O(W)`, together.

*Proof.* Every node reaches the accept state, since each node becomes a tail that a later node or the end of the pattern takes (§3), and every edge points forward; so every step of the depth-first walk lies on a path it completes. Charge each step to the first path completed below it. The steps charged to a path follow first successors from where it leaves the path before. Successors are listed in node order, and a join's first successor is the first node its group makes, which is no join, the group holding the join alone; a later join that gathers it comes after. On such a run joins and other nodes alternate at worst, and a path is charged at most twice its other nodes, plus one. A wildcard path's other nodes weigh at least their number, a literal path's number at most its bytes plus one, and the walk stops once the wildcard paths weigh more than the room together, or the literal paths take more of its bytes. ∎

**Lemma 5 (pickers and NFA).** The gates, the chain index and the graph NFA take `O(W log W)` time and `O(W)` memory.

*Proof sketch.* A gate reads a chain's two ends. The index reads each anchored element's strings once, at a cost linear in the element's weight (§5). It files at most the room's entries, and those of the chain that passes it, sorts them and lays them out in a table of twice their number. A `Small` NFA has at most 64 states and a `Wide` one rows within 4 KiB. A `Sparse` one lists each edge once and sorts each position's literal successors, and a literal sits in one list, its one predecessor's (§3.3). ∎

### 11.3 Calls

**Theorem 2.** `is_match(w)` and `match_dir(w)` take `O((n + 1) · W)` time and `O(W)` memory. `static_prefixes` copies `O(W)` bytes.

*Proof.* The prefilter compares at most 16 suffixes with the end of `w`. What follows depends on what compilation kept (§2); each case tests elements against segments, at the cost of §11.1.

- A lone line tests each element on one segment, and the globstar's absorption reads the middle once (§4.1). `match_dir` adds at most 90 tail alignments (`MAX_TAIL_CHECKS`, §4.4), each testing each element on one segment.
- A block tests its first element on each middle segment and, from each, the next elements on the segments that follow, so each element meets each segment at most once; the dot scan moves right only (§4.2).
- The literal set makes `O(log K)` comparisons of at most `n` bytes, and `match_dir` one more search.
- Gates try at most 64 chains, each once. The index makes at most nine lookups per segment it probes and walks a chain at most once in each of its four steps (§5); the chains weigh `O(W)` together (Lemma 4).
- A dense NFA step tests each active state once and ORs its row. A sparse step enters each position once, binary-searches its literal successors and tests the others, so each element at most once (§6), and runs the glue NFA once over the segment from all the entries it holds (§7.5). A step on segment `u` thus costs `O((|u| + 1) · W)`. `match_dir` adds at most 65 steps on wildcard-only segments, which test no bytes (§15.4).

A call keeps the NFA's sets, the in-segment NFA's and the glue NFA's sets, and the index's picks: `O(W)`. The static prefixes are those of Lemma 2. ∎

The bound is the one a simulation without backtracking cannot avoid, and the one RE2 and Rust's `regex` state. Its constant is the size of the pattern: a 4 KiB path against `{**,a}/` repeated to 57 KiB takes 0.66 s per call (Rust, 2026-10-27), and sixteen 255-byte segments against `{a*,b*}` repeated to 57 KiB 0.55 ms.

### 11.4 What the bounds assume

The bounds count each hash-table operation as `O(1)`. Three kinds of table hash the keys a pattern decides with a hash an adversary can compute: the graph walk's node and group tables (FNV, `graph.rs`), the chain index (a multiplicative hash of a segment's outer bytes or its polynomial hash, `chain.rs`), and in JS the maps keyed by small integers. Keys chosen to collide make one operation cost up to the table's size, and compilation `O(W²)` at worst. A call's probes of the index stay within Theorem 2, since a probe reads at most the `O(W)` entries. Factoring (Lemma 1) and the static prefixes (Lemma 2) use no hash for their bounds.

`tools/memory-check/tests/scaling.rs` checks the theorems on 19 shapes, the ones that broke an earlier bound among them: four times the pattern costs at most five times the memory and eight times the time, four times the path at most eight times the time.

## 12. Execution in JS

The JS implementation runs the same algorithm on JS strings (`charCodeAt`, `startsWith`, `endsWith`, `indexOf`), with no allocation per call. Byte exactness holds by construction: literal comparisons and the absorption of `*` and `**` do not depend on the count of UTF-16 units, and the only counting constructs, `?` (one byte) and negated classes (one byte), read a non-ASCII segment's UTF-8 length or run on that segment's bytes, so nothing is matched twice. A pattern with a non-ASCII byte compiles to byte mode outright. The equivalence of the string and byte modes is fuzzed by `packages/globstar/tests/string-mode.mjs`.

Where the twins differ, they differ in representation only: JS bit sets are int32 words, so its small graph NFA and small in-segment NFA hold 32 states; it has no `Wide` graph NFA (a graph past 32 states is sparse); and it finds a suffix group, however few the suffixes, by a binary search of their keys, since a typed table costs V8 more to allocate (about 200 ns) than the search saves.

## 13. The suffix prefilter

A *prefilter* for a language `L` is a predicate `A` with `A(w) = 0 ⇒ w ∉ L`; it may accept a `w ∉ L`, and it is cheaper than membership in `L`. The engine's prefilter is a literal suffix every match carries, compared at the end of the path before anything else runs.

### 13.1 Extraction

`segment/mod.rs::Graph::suffixes` reads the graph back from its ends, the nodes the accept state follows. An end gives its literal ends: a literal segment its bytes, a suffix set without a wildcard its segments, an affix its suffix, a suffix set its suffixes, pieces the bytes their last piece ends with. An end that guarantees no byte, as a `**` does, gives up the prefilter. An end that takes whole literal segments then reads back through the literal segments before it, each the one predecessor of the next (§3.3): a `**/` or a join ends the reading, and a segment of any other kind ends it after the `/` before the run. The `/` after a `**` thus belongs to that `**/`, which absorbs zero segments as well: the suffix of `a/**/b` is `b`, and `**/b` keeps matching `b`, while that of `src/*/index.ts` is `/index.ts`.

The suffixes of all ends form the set, in pattern order and without repeats, which the comparison tries in turn. Past 16 (`MAX_SUFFIX_SET`) the set gives way to the longest suffix all of them share, or to none when they share nothing, and the ends after it read no further back. `**/*.{ts,tsx,js}` has the set `{".ts", ".tsx", ".js"}`.

### 13.2 Comparison

`bytes::ends_with_pattern` compares the suffix with the end of the path, last byte first, under the byte rule of the literal engine (note 02, §2): a suffix `/` matches any byte of `Seps`, a separator byte in the suffix matches nothing, and letters fold under `case_insensitive`. Without the first rule, `*/main.ts` would reject `src\main.ts` on Windows.

### 13.3 Soundness

**Proposition 1.** For every pattern `P` and `w ∈ Σ*`, if the prefilter rejects `w` then `w ∉ L(P)`.

*Proof.* The extraction yields a set `S` such that every `w ∈ L(P)` ends with some `s ∈ S` under the byte rule: an accepted path ends at an end, whose element takes only segments that end with one of its literal ends, and every path to the end passes the segments read back before it. A suffix that all of `S` share, or a literal end that read no further back, ends every such `w` as well. The prefilter accepts `w` exactly when `w` ends with some element of `S`. If it rejects `w`, then `w` ends with no element of `S`, so `w ∉ L(P)`. ∎

The prefilter is not applied to `match_dir`: a directory path does not in general end with the pattern's suffix, which lies in the file segment, so the test would reject directories with matches below them.

### 13.4 Suffix rather than prefix

The engine reads from the left, so a prefix mismatch is found at the first differing segment or byte, and a prefix test would repeat that work. A suffix mismatch is found last: the NFA sees the end last, and a chain finds its tail after its head. An end-anchored test at the matcher's entry rejects a mismatched tail in `O(|suffix|)` without entering the engine. This is the common case on a walk, where most paths share the pattern's literal head and differ in the extension.

## 14. Static prefixes

### 14.1 Definition

For a pattern `P` with brace expansions `E(P)` (GLOB_SPEC §10.1), let `head(e)` be the longest literal prefix of the expansion `e` that ends at a segment boundary: every `w ∈ L(e)` starts with it, and either `head(e) = ε`, or every such `w` equals `head(e)` or continues after it with a separator. The static prefixes of `P` are the minimal elements of `{ head(e) : e ∈ E(P) }` under "is a segment-prefix of", an antichain. The separator after a head is not kept, a fully literal pattern is its own prefix, and the expansions of a union are its members'. The heads are bytes in the pattern's spelling: `/` for every separator, and the pattern's case.

### 14.2 Extraction

`segment/mod.rs::Graph::static_prefixes` walks the graph from the start, collecting the segments of literal nodes and of suffix sets without a wildcard, each segment of a set a branch, joined by `/`; a join passes the head on. At any other node (a wildcard, a `**`, a piece) and at the accept state the head read so far is recorded, without its trailing separators. Each path through the literal part of the graph records one head, and one buffer holds them all: a step names its node, where the head before it ends, and the segment the node before adds.

Paths multiply through suffix sets and joins: `{a,b}/{c,d}/…` doubles per segment. Every byte a head takes at a node, and every node it moves to, costs a unit of the room (§9), paid before the head is built; a head the room cannot pay for ends where it is, a shorter prefix that is still sound. The empty head covers every other, so recording it ends the reading.

With several heads, `dedupe` drops every head a shorter kept one covers (`src` covers `src/a`); the empty head covers all. It reads the heads in byte order, in which the kept heads that are prefixes of the current one form a chain ending at the last of them: only that one may cover the current head, since one before it would have covered it. The kept heads are then ordered by length and bytes.

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

*Proof.* Every `w ∈ L(P)` follows a path of the graph from the start to the accept state, and the walk follows that path through its literal part as far as the room lets it. The bytes it keeps are the segments that path's literal nodes and literal sets take before its first other node, each whole, so `w` starts with them at a segment boundary. A head cut short by the room is a segment-prefix of the full one and starts `w` as well, the empty head starts every path, and `dedupe` drops a head only when a shorter kept head covers it. ∎

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

A matcher with one chain and at most one globstar answers from the chain (§4.4), and one that keeps the literal set alone answers from the set (§4.3). Every other matcher splits `d` at separators and runs its NFA over the graph (§6); the active set is then classified with the accepting bit and the precomputed `reach1` mask:

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

One case is not provable: branches that split the names by length, `?/**/?` with `**/??*`, accept every name together, but the rule asks each `Wild` to accept every length on its own and answers `false`. A brace written within one segment (`**/{?,??*}`) is one `Wild` and is proven; union members are not merged there, so `**/?` with `**/??*` keeps two branches, as `{**/?,**/??*}` does, and is not.

### 15.5 Soundness

**Proposition 3 (no false pruning).** For every `P`, `d` and `w ∈ Σ*`, `match_dir(P, d) = Pruned` implies `d ∉ L(P)` and `d · "/" · w ∉ L(P)`.

*Proof.* `Pruned` is returned only when `exact` and `prefix` are both false. `exact = false` excludes `d`. `prefix = false` means that no active state can consume a separator and a continuation, possibly empty, to an accepting state. Neither `d` nor any path below it is accepted. ∎

**Proposition 4 (no false cover).** For every `P`, `d` and `s ∈ R`, `match_dir(P, d) ∈ { DescendAll, DescendAllAndMatch }` implies `d · "/" · s ∈ L(P)`.

*Proof.* Take any `s ∈ R` and the wildcard-only path `s'` of the same shape: the same number of segments, of the same lengths. Every transition a wildcard-only byte fires, a non-separator byte of `s` fires as well (`dot = true`, so no wildcard is dot-protected), and separators fire the same transitions in both. By induction the state set after `s` contains the state set after `s'`. The simulation visited `s'`, since it covers every shape, and found an accepting state there. ∎

Proposition 3 is what a walker relies on when it skips a directory; Proposition 4 is what a consumer that excludes files relies on when it skips one whole.

## 16. Source map

| Rust (`crates/globstar/src/`) | JS (`packages/globstar/src/`) | Role                                                                                |
| ----------------------------- | ----------------------------- | ----------------------------------------------------------------------------------- |
| `engine/segment/mod.rs`       | `engine/segment/index.js`     | the matcher: build, what a call runs (§2), `match_dir` (§15); the suffix prefilter (§13), the static prefixes (§14) |
| `engine/segment/graph.rs`     | `engine/segment/graph.js`     | the elements (§1), the graph walk (§3)                                              |
| `engine/segment/chain.rs`     | `engine/segment/chain.js`     | chains, gates, `match_dir` from a lone line (§4); the chain index (§5)              |
| `engine/segment/graph_nfa.rs` | `engine/segment/graph-nfa.js` | the graph NFA and the all-below search (§6, §15.4)                                  |
| `engine/segment/wild.rs`      | `engine/segment/wild.js`      | affixes, suffix sets, pieces, the in-segment NFA and the glue NFA (§7)              |
| `engine/segment/bits.rs`      | `engine/segment/bits.js`      | bit sets, byte sets of classes; in Rust, the byte sketch                            |
| `dir_match.rs`                | `dir-match.js`                | the six results and their three facts (§15.1)                                       |
