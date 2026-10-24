# 04. Static Prefixes and the Directory Predicate

The matcher exposes two results that let a walker skip directories: `static_prefixes(P)`, the literal heads every match carries, and `match_dir(P, d)`, a six-valued predicate on directory paths. This note states what each computes, how the implementation computes it, and why both are sound. Source: `engine/segment/literal_ends.rs::static_prefixes`, `engine/segment/mod.rs::SegmentMatcher::match_dir`, `dir_match.rs`.

## 1. Static prefixes

### 1.1 Definition

For a pattern `P` with brace expansions `E(P)` (GLOB_SPEC §10.1), let `head(e)` be the longest literal prefix of the expansion `e` that ends at a segment boundary: every `w ∈ L(e)` starts with it, and either `head(e) = ε`, or every such `w` equals `head(e)` or continues after it with a separator. The static prefixes of `P` are

```
SP(P) := the minimal elements of { head(e) : e ∈ E(P) }
         under "is a segment-prefix of"
```

an antichain. The separator after a head is not kept: the head of `src/*.ts` is `src`, and a fully literal pattern is its own prefix. The expansions of a union are its members', so `SP` of a union is the antichain of its members' heads. The heads are bytes in the pattern's spelling: `/` for every separator, and the pattern's case.

The boundary condition is what a walker needs: it seeds its traversal at `cwd · s`, and a directory can be read only at a segment boundary.

### 1.2 Extraction

`static_prefixes` reads the AST left to right, collecting bytes from literals and `/` from separators. A brace in the head is read branch by branch, each followed by what follows the brace, as a continuation rather than a copy. At the first other node (a wildcard, a `**`) the bytes are cut back to the last separator before it; a brace that the budget or the depth limit stops, and whose branches all start with a separator, ends a whole segment instead. Trailing separators are dropped. Each path through the braces of the head records one head.

Re-reading the continuation once per branch is the only part that can cost more than the pattern: `{a,b}{c,d}…` doubles per brace, and a brace before a long literal re-reads it once per branch. Each byte read and each branch taken therefore costs one unit of a budget of `4096 + 2 · weight(P)`, and braces nest at most 64 deep along one head; past either, the head ends at its last whole segment, a shorter prefix that is still sound. Reading every node once costs at most its weight, so the budget cuts only heads that re-read what a brace shares. Recording a head costs every byte read to reach it (the JS implementation copies them), and these could still sum to the square of the budget; past 64 budgets of such bytes the reading starts again as if at the depth limit, and the result is the one head before the first brace, which every expansion shares.

With several heads, `dedupe` sorts them by length and bytes and drops every head a shorter kept one covers (`src` covers `src/a`); the empty head covers all.

### 1.3 Examples

```
src/main.rs        →  { "src/main.rs" }
src/*.ts           →  { "src" }
src/**/*.ts        →  { "src" }
**/*.ts            →  { "" }
{src,test}/*.rs    →  { "src", "test" }
{src,src/a}/*.rs   →  { "src" }
{src,lib}/main.rs  →  { "lib/main.rs", "src/main.rs" }
{a/**,b}/c         →  { "a", "b/c" }
a{b,c}d/x          →  { "abd/x", "acd/x" }
```

### 1.4 Soundness

**Proposition 3.** For every `w ∈ L(P)` there is an `s ∈ SP(P)` such that `w` starts with `s` at a segment boundary.

*Proof.* Every `w ∈ L(P)` lies in `L(e)` for some expansion `e`, and the reading follows the branches of `e` through the braces of the head. The bytes it keeps come only from literal and separator nodes before the first other node, cut back to a segment boundary, so every `w ∈ L(e)` starts with them. A head cut short by the budget is a segment-prefix of `head(e)` and starts every such `w` as well, and `dedupe` drops a head only when a shorter kept head covers it. ∎

A walker may therefore seed one traversal per `s ∈ SP(P)`: a path that starts with no `s` is not in `L(P)`.

## 2. The directory predicate

### 2.1 Definition

For a pattern `P` and a directory path `d`, with `R` the set of paths of one or more non-empty segments (GLOB_SPEC §15.5),

```
below(d)        := Σ⁺                              if d = ""
                   { d · "/" · w : w ∈ Σ* }        otherwise
exact(P, d)     := d ∈ L(P)
prefix(P, d)    := below(d) ∩ L(P) ≠ ∅
all_below(P, d) := ∀ s ∈ R.  (d = "" ? s : d · "/" · s) ∈ L(P)

match_dir(P, d) := combine(exact, prefix, all_below)
```

with

```
combine(true,  _,     true)  = DescendAllAndMatch
combine(false, _,     true)  = DescendAll
combine(true,  true,  false) = DescendAndMatch
combine(true,  false, false) = Match
combine(false, true,  false) = Descend
combine(false, false, false) = Pruned
```

These are the definitions of GLOB_SPEC §15.2 and §15.5. `prefix` is over-approximated and `all_below` under-approximated, each on the consumer's safe side. At the root the implementation reports `prefix` as true for every pattern.

`Pruned` is the only result that lets a walker skip the directory. The two `DescendAll…` results let the opposite consumer, one that excludes files by pattern, skip the directory whole (§2.5).

### 2.2 Realization

A matcher with one chain and at most one globstar keeps no NFA: `chain_dir` reads from the chain the tokens the NFA would hold after `d` (a head position when `d` ends inside the head, the globstar when it has taken everything after the head, a tail position when the tail's first elements take the last segments) and classifies them as below. Every other matcher splits `d` at separators and runs its NFA over the graph (note 02, §6); the active set is then classified with the accepting bit and the precomputed `reach1` mask:

```
S       ← the state set after the segments of d
exact   ← S holds an accepting state
prefix  ← d = "" or S intersects reach1     (states that can consume at least one more segment to accept)
all     ← dot and prefix and covers_below(S)
result  ← combine(exact, prefix, all)
```

The chain path and the one-word NFA (64 states in Rust, 32 in JS) allocate nothing per call. Past that, Rust allocates the NFA's sets per call and JS keeps them on the matcher.

### 2.3 The prefilter is not used

`match_dir` does not apply the suffix prefilter of note 03: a directory path does not in general end with the pattern's suffix, and the test would reject directories with matches below them.

### 2.4 Negated patterns

A negated pattern returns `DescendAndMatch` when the directory matches and `Descend` otherwise (GLOB_SPEC §15.4): its language is the complement of another, and the deeper paths, unseen, cannot be excluded from that complement.

Rust's `Glob::union` rejects negated members (`NegatedInUnion`); the JS matcher accepts them, and a negated member contributes its complement (GLOB_SPEC §16.1). When a member is negated, `exact` is the union's own answer, `prefix` is true, and the positive members alone decide `all_below`:

```
match_dir(union with a negated member, d) = combine(∃ member matches d, true, all_below of the positive members)
                                          ∈ { Descend, DescendAndMatch, DescendAll, DescendAllAndMatch }
```

so no directory is pruned. For instance, `["a/**", "!b"]` gives `DescendAllAndMatch` at `a`, since `a` is not `b`, and `["!b"]` gives `Descend` at `b`. The walker (WALKER_SPEC §4.2) routes `!`-prefixed patterns into its ignore set, so its matchers hold positive patterns only; the conservative case concerns direct callers of the matcher.

### 2.5 All-below

The implementation decides `all_below` by simulation. A *wildcard-only* byte is one that `?`, `*` and `**` consume and that every literal and class rejects; a wildcard-only path is built from such bytes and separators. From the state set that `d` leaves, the simulation feeds wildcard-only segments and requires an accepting state after each:

```
S ← the state set after the segments of d
repeat
    S ← step S over a wildcard-only segment
        (a globstar absorbs it; a Wild consumes it iff its wildcards alone
         accept every non-empty segment; a Lit never does)
    if S holds no accepting state:  return false
    if S did not change:            return true
```

Tokens move right along a sequence or stay on a globstar, so the loop ends within the number of states; it is capped at 65 steps (GLOB_SPEC §15.5). The bit per `Wild`, whether its wildcards alone accept every non-empty segment, is precomputed: `*` and `?*` have it, `*.js` and `?` do not, and a `Generic` in-segment NFA obtains it from one pass over its states in order. Every edge but a star's loop points to a later state, so when the pass reaches a state the lengths of the wildcard-only strings that reach it are known: one bit per length below 64 and every length from a tail on. A `?` adds one to each length, a `*` extends from the least length on, and the bit holds when every length from 1 is present. A tail past 64 is not followed: a brace of 80 `{?,}` beside 70 `?` and a `*` accepts every length, yet is not known to.

A segment that a brace cut into glued pieces (note 02, §3.4) is no element of its own. The pieces a set holds enter the in-segment NFA that glues them, at once, and the same pass over its states, from those entries, tells each node where they may end a segment whether the wildcards alone accept every name there; the loop fires those nodes. A segment of a given length reaches those nodes for every segment of that length, so the nodes stand for the segments through the pieces as a whole segment's state does, and the answer is the one the copied segments would give.

On a lone chain the simulation has a closed form, which `chain_dir` uses: the globstar is alive after `d`, every tail element is such a `Wild`, and every shorter tail token is alive as well (the tail's first `j` elements take the last `j` segments, the globstar released before them) for `1 ≤ j < tail length`, within a budget of 4,096 element comparisons.

Under `dot = false` the answer is `false` without simulation: `**` does not match a dot-led name, so some `d/.x` escapes.

One case is not provable: branches that split the names by length, `?/**/?` with `**/??*`, accept every name together, but the rule asks each `Wild` to accept every length on its own and answers `false`. Union members that merge into one segment (`**/?` with `**/??*` factors to `**/?{,?*}`) are one `Wild` and are proven; factoring merges members and not the braces a pattern writes, so `{**/?,**/??*}` keeps two branches and is not.

## 3. Walker integration

WALKER_SPEC §6 and §7 state the walker's rules. From the matcher's side:

- `static_prefixes()` is read once, to produce the seed frames, each reached segment by segment through the per-entry rule.
- `match_dir(child)` is asked once per discovered directory and gates the descent; the JS walker returns files only and leaves `exact` unused, while the Rust walker yields a matching directory as well.
- `is_match(child)` is asked once per discovered file.
- `ignore.is_match(child)` is asked once per entry of either kind, before any other work; `ignore.is_match(child · "/")` once per directory; and under `dot: true`, `ignore.match_dir(child)` once per directory worth entering, whose `DescendAll…` result skips the subtree (§2.5).

## 4. Soundness

**Proposition 4 (no false pruning).** For every `P`, `d` and `w ∈ Σ*`,

```
match_dir(P, d) = Pruned   ⇒   d ∉ L(P)  ∧  d · "/" · w ∉ L(P)
```

*Proof.* `Pruned` is returned only when `exact` and `prefix` are both false. `exact = false` excludes `d`. `prefix = false` means that no active state can consume a separator and a continuation, possibly empty, to an accepting state. Neither `d` nor any path below it is accepted. ∎

This is the property a walker needs: it may skip any subtree whose root receives `Pruned`.

**Proposition 5 (no false cover).** For every `P`, `d` and `s ∈ R`,

```
match_dir(P, d) ∈ { DescendAll, DescendAllAndMatch }   ⇒   d · "/" · s ∈ L(P)
```

*Proof.* Take any `s ∈ R` and the wildcard-only path `s'` of the same shape: the same number of segments, of the same lengths. Every transition a wildcard-only byte fires, a non-separator byte of `s` fires as well (`dot = true`, so no wildcard is dot-protected), and separators fire the same transitions in both. By induction the state set after `s` contains the state set after `s'`. The simulation visited `s'`, since it covers every shape, and found an accepting state there. ∎

This is the property an excluding consumer needs: it may skip any directory that receives a `DescendAll…` result, and no path it hides escapes the pattern.

## 5. A worked example

Layout on disk, relative to `cwd = /proj`:

```
/proj/
├── src/
│   ├── main.ts
│   ├── components/
│   │   ├── Button.tsx
│   │   └── Card.tsx
│   └── cli/
│       └── run.ts
├── tests/
│   └── smoke.ts
└── node_modules/
    └── lodash/ … (3,000 files)
```

Pattern: `src/**/*.ts`, with `SP = { "src" }`.

**Seeding.** The one prefix `src` is reached as the only entry of `/proj`: `lstat("/proj/src")` is a directory, no ignore matches it, and `match_dir("src")` allows descent, so the frame `/proj/src` is seeded. `/proj/tests` and `/proj/node_modules` are never read.

**Frame `/proj/src`.** The listing holds `main.ts`, `components` and `cli`. `main.ts` is a file and `is_match("src/main.ts")` holds (the globstar takes nothing), so `/proj/src/main.ts` is returned. `components` and `cli` are directories; `match_dir` gives `Descend` for both (the head `src` is taken, the globstar is alive, the tail `*.ts` can still follow), so both are queued.

**Frame `/proj/src/components`.** `Button.tsx` and `Card.tsx`: the suffix prefilter rejects both on their last byte, and the engine does not run.

**Frame `/proj/src/cli`.** `run.ts` passes the prefilter, the chain matches (head `src`, tail `*.ts`, the globstar takes `cli`), and `/proj/src/cli/run.ts` is returned.

The walk reads three directories. A walk from `cwd` without seeding would have read every directory under `/proj`, the `lodash` subtree included.

**With an ignore set.** With `ignore: ["**/*.test.ts"]` and a file `src/foo.test.ts`, the walker compiles a second matcher and asks `ignore.is_match` first for each entry; `src/foo.test.ts` is skipped without further work. Under `dot: true` the walker also asks `ignore.match_dir(d)` for each directory it would enter: a directory where `matches_all_below` holds is skipped whole, so an entry such as `**/node_modules/**/*`, which no cheaper test catches, still prunes `node_modules`. `**/*.test.ts` covers no directory, so every entry is tested individually. Under `dot: false` the test is not made: a `**` never covers the dot-led names below (GLOB_SPEC §15.5), which a positive pattern may still name.

## 6. Source map

| Symbol                                                                                      | File                                                                     |
| ------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `static_prefixes`, `dedupe`                                                                 | `engine/segment/literal_ends.rs`                                         |
| `SegmentMatcher::match_dir`                                                                 | `engine/segment/mod.rs`                                                  |
| `Nfa::match_dir`, `Nfa::covers_below`, `SegmentMatcher::chain_dir`                          | `engine/segment/graph_nfa.rs`, `engine/segment/chain.rs`                 |
| `DirMatch`, `DirMatch::matches`, `DirMatch::may_match_below`, `DirMatch::matches_all_below` | `dir_match.rs`                                                           |
| walker glue (JS)                                                                            | `packages/globstar-walk/src/walk.js::initFromPrefixes`, `processDirents` |

## 7. References

- Hopcroft, J. E., Motwani, R. and Ullman, J. D. (2006). *Introduction to Automata Theory, Languages, and Computation*, 3rd ed. Reverse reachability (the `reach1` mask) is textbook material.
- The six-valued directory predicate is this project's own. `globset` has no directory query; `ignore`'s result speaks of the path itself; `walkdir`'s `filter_entry` takes a boolean from its caller.
