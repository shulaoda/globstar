# 04 — Static-prefix Extraction and `match_dir`

The matcher exposes two outputs that drive directory-level pruning:

- `static_prefixes(P)`, the deepest segment-bounded literal prefix per brace expansion of `P`;
- `match_dir(P, d)`, a six-valued predicate over directory paths classifying whether the pattern can match `d` itself, descendants of `d`, both, or neither — and whether it matches every descendant.

Implementation:

- `engine/segment/facts.rs::static_prefixes` — the prefix extraction pass.
- `engine/segment/mod.rs::SegmentMatcher::match_dir` — the realization.
- `dir_match.rs` — the `DirMatch` algebraic datatype.

This note formalizes both and documents how the walker (`references/spec/WALKER_SPEC.md`) consumes them.

## 0. Pipeline

```
   pattern ──► matcher.compile ──► matcher  ──► static_prefixes()
                                          │
                                          └──► match_dir(d)
                                                    │
                                                    ▼
                                    DirMatch ∈ {Pruned, Descend, Match, DescendAndMatch,
                                                DescendAll, DescendAllAndMatch}

Walker startup:
   for each prefix p in matcher.static_prefixes():
       walk p part by part from cwd, each part judged by the per-entry
       rule below as its folder's only entry (lstat stands in for the
       listing); a part that gives no frame drops the seed, the last
       part's result is the seed's: a file to emit, or a frame to read

Walker per directory frame (absolute, relative):
   for each entry in fs.readdir(absolute):
       child_rel = relative ++ "/" ++ entry.name
       if ignore.match(child_rel):     skip
       else if entry is directory:     # a symlink takes its target's type
           if ignore.match(child_rel ++ "/"):                  skip
           dm = matcher.match_dir(child_rel)
           if not DirMatch::may_match_below(dm):              skip
           if dot and ignore.match_dir(child_rel) covers all below: skip
           enqueue child frame        # unless the link closes a cycle
           # the JS walker emits files only; Rust also yields the folder
           # when DirMatch::matches(dm), or a pattern ending in `/` matches it
       else if matcher.match(child_rel):
           emit child path
```

Two outputs from the matcher drive the walker: `static_prefixes` jumps the traversal head past the literal prefix that every match must carry, and `match_dir` answers "could this directory's subtree contain any match?" once per discovered directory.

## 1. Static prefixes

### 1.1 Definition

For a pattern `P` with brace expansions `E(P)` (GLOB_SPEC §10.1), let `head(e)` be the longest literal prefix of the expansion `e` that ends at a path-segment boundary: every `w ∈ L(e)` starts with it, and it is `ε` or every such `w` equals it or goes on after it with a separator. The _static prefixes_ of `P` are

```
SP(P) := the minimal elements of { head(e) : e ∈ E(P) }
         under "is a segment-prefix of"   (an antichain)
```

The separator after a head is not kept: the head of `src/*.ts` is `src`, and a fully literal pattern is its own prefix (`src/main.rs`). For a union the expansions are its members', so `SP` of a union is the antichain of its members' heads. The heads are bytes in the pattern's own spelling: `/` for every separator and the pattern's case.

The segment-boundary requirement matters because the walker uses `SP(P)` to seed traversal at `cwd ++ s`, and `fs.readdir` only operates on directories. A non-segment-aligned prefix would force the walker to filter mid-segment, which it cannot.

### 1.2 Extraction

`static_prefixes` reads the AST left to right, accumulating bytes from literals and `/` from separators. A brace in the head is read branch by branch, each followed by what follows the brace (a continuation, so nothing is copied). At the first other node (a wildcard, a `**`) the bytes are cut back to the last separator before it; a brace the budget or the depth limit stops, whose every branch starts with a separator, ends a whole segment instead. Trailing separators are dropped. Each path through the head braces records one head.

Re-reading the continuation once per branch is what could cost more than the pattern: `{a,b}{c,d}…` doubles per brace, and a brace before a long literal copies it once per branch. So every byte read and every branch taken costs one unit of a budget of `4096 + 2·weight(P)`, and braces nest at most 64 deep along one head; past either, the head ends at its last whole segment — a shorter prefix, still sound. Reading every node once costs at most its weight, so the budget only ever cuts heads that re-read what a brace shares. Recording a head costs every byte read to reach it, the head and what was cut back from it (the JS slice copies them all), while its branch pays one unit, so these could still add up to the square of the budget: past 64 budgets of such bytes, the reading starts again as if at the depth limit, and the one head before the first brace, which every expansion shares, is the list.

When there are several prefixes, a dedupe step (`facts.rs::dedupe`) sorts them by (length, bytes) and drops any prefix that a shorter kept one already covers (`src` covers `src/a`); an empty prefix covers all.

The work and the output are `O(weight(P))` before the dedupe. Output is `Box<[Box<[u8]>]>`, cached on the matcher.

### 1.3 Examples

```
src/main.rs        →  ["src/main.rs"]    (fully literal)
src/*.ts           →  ["src"]
src/**/*.ts        →  ["src"]
**/*.ts            →  [""]               (no literal head)
{src,test}/*.rs    →  ["src", "test"]
{src,src/a}/*.rs   →  ["src"]            (src/a is covered by src)
{src,lib}/main.rs  →  ["lib/main.rs", "src/main.rs"]
{a/**,b}/c         →  ["a", "b/c"]
a{b,c}d/x          →  ["abd/x", "acd/x"]
```

### 1.4 Properties

**Proposition 4.** For every `w ∈ L(P)`, there exists `s ∈ SP(P)` such that `w` starts with `s`.

_Proof sketch._ Every `w ∈ L(P)` lies in `L(e)` for some expansion `e`, and the read follows `e`'s branches through the head braces. The bytes it keeps come only from literal and separator nodes before the first other one, cut back to a segment boundary, so they start every `w ∈ L(e)`. A head cut short by the budget is a segment-prefix of `head(e)` and starts every such `w` too, and the dedupe only drops a head when a shorter kept one covers it. ∎

The corollary is that the walker may safely seed a separate traversal frame for each `s ∈ SP(P)`: paths that do not start with any `s` are excluded from `L(P)`, so they need not be visited.

## 2. The `DirMatch` predicate

### 2.1 Definition

```rust
pub enum DirMatch {
    Pruned,
    Descend,
    Match,
    DescendAndMatch,
    DescendAll,
    DescendAllAndMatch,
}
```

Given `P` and a directory path `d`, the walker uses

```
below(d)           := Σ⁺                              if d = ""   (the walk root)
                      { d ++ "/" ++ w : w ∈ Σ* }      otherwise
exact_match(P, d)  := d ∈ L(P)
prefix_match(P, d) := below(d) ∩ L(P) ≠ ∅
all_below(P, d)    := ∀ s ∈ R.  (d = "" ? s : d ++ "/" ++ s) ∈ L(P)      (R: one or more non-empty segments)
match_dir(P, d) := combine(exact_match(P, d), prefix_match(P, d), all_below(P, d))
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

These are the definitions of `references/spec/GLOB_SPEC.md` §15.2 and §15.5. At the root the engines report `prefix_match` as true for every pattern.

`Pruned` is the only verdict that licenses the walker to skip `readdir(d)`. The two `DescendAll…` verdicts license the opposite kind of consumer — one that EXCLUDES files by glob, such as a watcher — to skip `d` whole (§2.5).

### 2.2 Segment realization

A matcher with one chain and at most one globstar — the common case — keeps no NFA: `chain_dir` reads the tokens the NFA would hold after `d` off the chain (a head position when `d` ends inside the head, the globstar when it took everything after the head, a tail position when the tail's first elements take the last segments) and classifies them as below. Any other SegmentMatcher splits `d` at path separators and runs its element-position NFA. The active position set — a bit set of one `u64` up to 64 states or dense word rows to about 170 states, and past that (or past a literal fan-out of 48) a sparse list of states and positions — is then classified with the accept bit and the precomputed `reach1` mask:

```
S ← run the segments of d
exact  ← S intersects accepting positions
prefix ← d = "" or S intersects reach1 (positions that can consume at least one descendant)
all    ← dot and prefix and covers_below(S)      (§2.5)
verdict ← combine(exact, prefix, all)
```

This is the production path. It answers the directory query without rebuilding a byte-level automaton. The chain path and the one-word NFA (64 states in Rust, 32 in JS) allocate nothing per call. Past that Rust allocates the NFA's few sets per call (dense rows up to 170 states, sparse lists beyond); JS keeps its sparse sets on the matcher and allocates nothing.

### 2.3 The suffix prefilter is not used

Unlike `is_match`, `match_dir` does not consult the suffix prefilter (§03; Rust `SegmentMatcher::prefilter`, JS `_prefilter`). A directory prefix in general does not end with the pattern's suffix — the pattern's literal tail lives in the file segment, not the directory segment — so the prefilter would over-reject. The engine runs unfiltered for `match_dir`.

### 2.4 Negated patterns

A lone negated pattern returns `DescendAndMatch` when the directory itself matches and `Descend` otherwise, in both runtimes (`references/spec/GLOB_SPEC.md` §15.4). A negated pattern matches paths _not_ in some sublanguage, and the walker has not yet seen the deeper paths; it cannot rule out their membership in the complement.

Rust's `Glob::union` rejects `!` members (`NegatedInUnion`). The JS multi-pattern matcher (`compileMatcher` in `packages/globstar/src/glob.js`, built by `makeMatcher`) accepts them: a negated member matches every directory outside its sublanguage and contributes may-below (GLOB_SPEC §16.2). So when any member is negated, `matches` is the union's own answer (a negated member counts), may-below is forced true, and the positive members alone give the all-below bit:

```
match_dir(P with negations, d) = combine(any member matches d, true, positive all-below)
                               ∈ {Descend, DescendAndMatch, DescendAll, DescendAllAndMatch}
```

i.e. pruning is suppressed: the verdict is never `Pruned` or `Match`. For example `compileMatcher(["a/**", "!b"]).matchDir("a")` is `DescendAllAndMatch` (`a` is not `b`, so the negated member matches it), and `compileMatcher(["!b"]).matchDir("b")` is `Descend`. Pruning is restored only when no negative member exists.

This is the formal counterpart of `references/spec/GLOB_SPEC.md` §15.4 and §16.2. The walker (§ `references/spec/WALKER_SPEC.md` §4) splits `!`-prefixed input patterns into its own ignore set, so the matcher receives only positive patterns at the walker layer; the conservative case applies only to direct standalone callers.

### 2.5 All-below

`prefix_match` over-approximates: `Descend` means a descendant MAY match. `all_below` under-approximates: it is set only when every descendant provably matches. Both err on the consumer's safe side.

The engine decides it by simulation. A _wildcard-only_ byte is one that `?`, `*` and `**` consume and that every literal and class rejects; a wildcard-only path is built from such bytes and separators, so only the lengths of its segments tell two apart. Starting from the state set that `d` left behind, the engine feeds every wildcard-only path in `R` and requires an accept after each segment.

First a wildcard-only segment of any length, stepped until the set settles:

```
S ← the state set after the segments of d
repeat:
    S ← step S over a wildcard-only segment
        (globstars absorb it; a Wild consumes it iff its wildcards
         alone match every non-empty segment; a Lit never does)
    if S has no accept: return false
    if S did not change: return true
```

Tokens only move right along a sequence or sit on a globstar, so the loop ends within the number of states; it is capped at 65 steps (GLOB_SPEC §15.5). The per-`Wild` bit is precomputed: `*` and `?*` have it, `*.js` and `?` do not, and a `Generic` in-segment NFA gets it from the same simulation run over its `Any` states.

Where that answers `false`, segments of each length may still prove the flag: `?` takes every one-byte name and `??*` every longer one, so `{**/?,**/??*}` covers every name though no element does alone. Each `Wild` knows the lengths whose every name its wildcards alone take: bit `L` for 1 ≤ L ≤ 31 and "every length from `from` on" (≤ 32), read off its affixes (`?` repeated with nothing literal around it, a `*` among them for the open form) or off the same simulation of a `Generic` NFA, one byte at a time. With K the longest length some element singles out, lengths 1 to K and "longer than K" are the length classes: within one, every element takes every name or none. The engine searches the fired sets, the states a class's segments take, from the set `d` left:

```
for each set S reached (the set d left, then what each fired set enters):
    T ← the states of S that take every segment of some class
    if T was seen: continue          (it fires what it fired before)
    for each class c:
        F ← the states of T that take every segment of class c
        if F is empty: return false
        if F is new: queue it
for each F queued:
    S ← what F enters
    if S has no accept: return false
return true when the queue is done
```

A real segment fires at least what its class fires, so by induction every real path below `d` reaches, after each segment, a superset of a set the search visited, each holding accept. An empty fired set enters nothing, so it answers `false` at once. The fired sets are the same in every NFA kind (the dense and sparse ones step the same sets of states), and the search visits them in one order, so every NFA gives the same answer. It gives up (`false`) past 64 fired sets, past 512 firings of a class, or once the fired sets hold 64 states per state of the NFA, counted at each firing: it costs about what 64 segments do.

On a lone chain the simulation has a closed form, which `chain_dir` uses: the globstar is alive after `d`, every tail element is such a `Wild`, and every shorter tail token is alive as well (the tail's first j elements take the last j segments, the globstar released before them), for 1 ≤ j < tail length, within a budget of 4,096 element compares.

Under `dot=false` it returns `false` without simulating: `**` does not match a dot-led name, so some `d/.x` always escapes.

Branches that split the names by length — `?/**/?` with `**/??*` at `a`, `{**/?,**/??*}` anywhere — are proven by the length search. What stays unprovable is past its bounds: an element that singles out a length past 31 bytes (`?` repeated 33 times, with `*` or not) counts as taking no length, and a pattern whose fired sets pass the caps answers `false`.

## 3. Walker integration

The walker's responsibilities relative to this module are documented in `references/spec/WALKER_SPEC.md`. From the matcher's side:

- `static_prefixes()` is consumed once — at construction in JS (`prepare()` → `initFromPrefixes`), on the first `next()` in Rust — to produce seed frames, each reached part by part through the per-entry rule.
- `match_dir(child_rel)` is consulted once per discovered directory during `processDirents`. The return value gates the descend decision; the JS walker emits files only and leaves the `Match` flag unused, while the Rust walker yields a matching folder too.
- `is_match(child_rel)` is consulted once per discovered file.
- `ignore.is_match(child_rel)` is consulted once per discovered entry of either kind, before any other matcher work; `ignore.is_match(child_rel ++ "/")` once per directory (an entry that ends in `/` or `/**` prunes it, WALKER_SPEC §3.2); and under `dot: true`, `ignore.match_dir(child_rel)` once per directory worth descending, whose `DescendAll…` verdict skips the subtree (§2.5).

## 4. Properties

**Proposition 5 (no false pruning).** For every `P`, `d`, and `w ∈ Σ*`:

```
match_dir(P, d) = Pruned   ⇒   d ∉ L(P) ∧ ∀ w. d ++ "/" ++ w ∉ L(P)
```

_Proof sketch._ `Pruned` is returned only when `exact = false ∧ prefix = false`. `exact = false` excludes `d` itself. `prefix = false` means no active segment/NFA position can consume a separator and a descendant path (possibly empty) to acceptance. Together, neither `d` nor any descendant is accepted. ∎

This is the soundness property the walker requires: it may safely prune any subtree whose root receives `Pruned`.

**Proposition 6 (no false cover).** For every `P`, `d`, and `s ∈ R`:

```
match_dir(P, d) ∈ {DescendAll, DescendAllAndMatch}   ⇒   d ++ "/" ++ s ∈ L(P)
```

_Proof sketch._ Take any real `s` and the wildcard-only path `s'` of the same shape — the same number of segments, the same lengths. Every transition a wildcard-only byte fires, a real non-separator byte fires too (`dot=true`, so no wildcard is dot-protected), and separators fire the same transitions in both. By induction the state set after `s` contains the one after `s'`. The simulation visited `s'` — the any-length one covers every shape at once, the length search every sequence of length classes, and a wildcard-only segment fires the same states as any other of its class — and found an accept there. ∎

This is the soundness property an excluding consumer requires: it may skip any directory that receives a `DescendAll…` verdict, and no path it hides escapes the pattern.

## 5. Worked example

Layout on disk (relative to `cwd = "/proj"`):

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
    └── lodash/ ... (3000 files)
```

Pattern: `src/**/*.ts`. Compiled matcher has `static_prefixes = [b"src"]` (the literal head before the first non-literal node).

### 5.1 Seeding

```
matcher.static_prefixes() = [ b"src" ]

walker iterates:
  prefix = b"src", one part: "src"
  lstatSync("/proj/src") → directory, judged as the only entry of "/proj"
  ignore.match("src") = false (no ignores)
  DirMatch.mayMatchBelow(matcher.match_dir(b"src")) = true
  push frame { absolute: "/proj/src", relative: "src" }
```

`/proj/tests` and `/proj/node_modules` are NEVER touched: no `static_prefix` covers them. Three thousand lodash files are skipped before a single `readdir`.

### 5.2 First frame: `/proj/src` (relative=`src`)

```
fs.readdir("/proj/src", withFileTypes=true) →
  [Dirent{name:"main.ts", file},
   Dirent{name:"components", dir},
   Dirent{name:"cli",        dir}]

For "main.ts":
  child_rel = "src/main.ts"
  ignore.match(...)         = false  (no ignores)
  isDirectory               = false
  matcher.match("src/main.ts") = true  (matches src/**/*.ts via empty middle)
  → emit "/proj/src/main.ts"

For "components":
  child_rel = "src/components"
  isDirectory = true
  matcher.match_dir("src/components") = ?
    chain_dir: the head `src` is taken; the globstar is alive after it,
      and the tail `*.ts` can still follow (deeper paths can still match)
    verdict = Descend
  → enqueue frame { absolute: "/proj/src/components", relative: "src/components" }

For "cli": (similar to "components")
  matcher.match_dir("src/cli") = Descend
  → enqueue frame { absolute: "/proj/src/cli", relative: "src/cli" }
```

After this frame: `out = ["/proj/src/main.ts"]`, two new frames queued.

### 5.3 Second frame: `/proj/src/components`

```
fs.readdir(...) → [Button.tsx, Card.tsx]

For "Button.tsx":
  child_rel = "src/components/Button.tsx"
  isDirectory = false
  matcher.match("src/components/Button.tsx"):
    suffix is ".ts" → ends_with(".ts") fails in the prefilter.
    prefilter(path) returns false (suffix mismatch on last byte 'x' vs 's').
  → match = false; do not emit

For "Card.tsx": same, do not emit.
```

### 5.4 Third frame: `/proj/src/cli`

```
fs.readdir(...) → [run.ts]

For "run.ts":
  child_rel = "src/cli/run.ts"
  isDirectory = false
  prefilter("src/cli/run.ts") → ends with ".ts" → true
  matcher.match runs the chain: head `src`, tail `*.ts`, the globstar takes `cli`
  → emit "/proj/src/cli/run.ts"
```

Final result: `["/proj/src/main.ts", "/proj/src/cli/run.ts"]`.

The walker performed exactly 3 `readdir` calls (one per directory under `src/`). A naive walk would have read every folder under `/proj`, the whole lodash subtree included. The savings come entirely from `static_prefixes` (skipping `/proj/tests` and `/proj/node_modules` upfront) and from the suffix prefilter inside `matcher.match` (rejecting `.tsx` files in `O(1)` per file instead of running the full matcher).

### 5.5 With an ignore: `glob("src/**/*.ts", { ignore: ["**/*.test.ts"] })`

Same setup, plus `src/foo.test.ts`. The walker compiles a second matcher (`ignore`) for `**/*.test.ts`. At each entry the walker calls `ignore.match(child_rel)` first; matching paths skip without recursion.

```
For "src/foo.test.ts":
  ignore.match("src/foo.test.ts")   = true  → skip without emit
For "src/cli/run.ts":
  ignore.match("src/cli/run.ts")    = false → continue (emitted via main matcher)
```

Under `dot: true` the walker also asks `ignore.match_dir(d)` for each directory it would descend: one where `matches_all_below` holds (§2.5) is skipped whole, so an entry like `**/node_modules/**/*`, which no cheaper check catches, still prunes `node_modules` whole. `**/*.test.ts` covers no directory (every path below must end in `.test.ts`), so here each entry is still tested with `match`. Under `dot: false` the check is not made: a `**` never covers the dot-led names below (GLOB_SPEC §15.5), and a positive pattern may still name one.

## 6. Source map

| Symbol                                                                              | File                                                                     |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `static_prefixes`                                                                   | `engine/segment/facts.rs`                                                |
| `SegmentMatcher::match_dir`                                                         | `engine/segment/mod.rs`                                                  |
| `Nfa::match_dir`, `Nfa::covers_below`, `SegmentMatcher::chain_dir`                  | `engine/segment/nfa.rs`, `engine/segment/chain.rs`                       |
| `DirMatch`, `DirMatch::matches`, `DirMatch::may_match_below`, `DirMatch::matches_all_below` | `dir_match.rs`                                                           |
| Walker glue (JS)                                                                    | `packages/globstar-walk/src/walk.js::initFromPrefixes`, `processDirents` |

## 7. References

- Hopcroft, J. E., Motwani, R., Ullman, J. D. _Introduction to Automata Theory, Languages, and Computation_. Reverse reachability and quotient automata are textbook material; the `reach1` mask (`engine/segment/nfa.rs`) is a direct application.
- The six-valued `DirMatch` is this project's own. `globset` has no directory query; `ignore`'s `Match` (none / ignore / whitelist) speaks only of the path itself; `walkdir`'s `filter_entry` takes a plain `bool` from its caller. The literature for prefix-based pruning of regular-expression search lives in index-pattern work (e.g. _Cox, R., Russ Cox's regular expression notes_) and incremental DFA matching.
