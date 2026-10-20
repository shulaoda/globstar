# 03 — Literal Suffix Prefilter

The matcher composes a fast prefilter with the exact engine (`SegmentMatcher`). The prefilter rejects byte strings that cannot match the pattern by checking a guaranteed literal suffix; it is sound (no false negatives) and approximate (some false positives).

Implementations: `crates/globstar/src/engine/segment/facts.rs` and `packages/globstar/src/engine/segment/facts.js`.

## 0. Pipeline

```
AST      ──►  trailing(nodes)            ──►  one suffix
   │          (right-to-left over literal/separator nodes)
   │                                                  │
   └────►  if that suffix is empty AND the tail is a brace of ≤ 16 branches:
              suffix_set(nodes)          ──►  one suffix per branch

                                                  │
                                                  ▼
                          suffixes(body): a list of byte strings (empty = no prefilter)
                                                  │
                                                  │  carried inside the matcher
                                                  ▼
   ┌──────────────────────────────────────────────────────┐
   │ is_match(path):                                      │
   │   if !accept(path) { return false }                  │  ← O(|suffix|), end-anchored
   │   else { run engine }                                │
   └──────────────────────────────────────────────────────┘
```

`accept(path)` is the check: `SegmentMatcher::prefilter` in Rust (`engine/segment/mod.rs`), `_factsAccept` in JS (`engine/segment/index.js`). It is a separator-aware `ends_with`: a `'/'` byte in the suffix matches any `Seps` byte in the path (so `*/main.ts`, suffix `/main.ts`, rejects `src\foo.js` on Windows and accepts `src\main.ts`).

When the trailing literal/separator run is non-empty it is the single anchor. Otherwise, when the pattern ends in a brace of at most 16 branches (e.g. `**/*.{ts,tsx,js}`), `suffix_set` derives one anchor per branch and `accept` succeeds if `path` ends with any of them.

## 1. The prefilter abstraction

A _prefilter_ for a language `L ⊆ Σ*` is a predicate `P : Σ* → {0, 1}` such that

```
P(w) = 0   ⇒   w ∉ L
```

(or, equivalently, `w ∈ L ⇒ P(w) = 1`). `P` is allowed to over- accept: there may exist `w` with `P(w) = 1 ∧ w ∉ L`. The point of a prefilter is that `P` is much cheaper to evaluate than membership in `L`, so a matcher can short-circuit on `¬P(w)` and skip the exact engine.

The pattern in our setting is generic enough to call by its literature name: _filter-then-verify_. A common instantiation in text indexing replaces a regular language match with a fast literal search (`memmem` / Aho-Corasick); in our setting we use a single end-anchored byte comparison.

## 2. The suffix prefilter

For a pattern `P` with language `L(P)`, define

```
suffix(P) := the trailing literal run of P: an s ∈ Σ* such that
             every w ∈ L(P) ends with s (not always the longest
             such s: `{xa,ya}c` gives `c`)
```

(`s` may be `ε` if no non-empty common suffix exists). Then

```
prefilter_P(w) := w ends with suffix(P)
```

is a sound prefilter: any `w ∈ L(P)` ends with `suffix(P)` by definition. The implementation stores `suffix(P)` as one byte string and evaluates `prefilter_P` by separator-aware `ends_with` (§4).

For patterns with no such suffix whose tail is a brace of at most 16 branches (`MAX_SUFFIX_SET`), we extract a _suffix set_ instead:

```
suffix_set(P) := { s_i : s_i is the suffix contributed by branch i }
prefilter_P(w) := ∃ s ∈ suffix_set(P). w ends with s
```

This handles common idioms like `**/*.{ts,tsx,js}` whose single longest common suffix is empty (`.ts` and `.tsx` end differently) but whose suffix-set is `{".ts", ".tsx", ".js"}`, providing a stronger filter. A brace with more than 16 branches gets no prefilter. When its branches are literals, the engine indexes them itself (`AffixIndex`, §02, at any count); a brace with a wildcard branch, or braces whose product passes 1,024 names, is matched by the segment's NFA instead.

## 3. Why suffix and not prefix

The engine starts from the left: a chain matches its head first, the NFA reads left to right. A prefix mismatch is detected at the first conflicting segment or byte, so a separate prefix prefilter would duplicate work.

A suffix mismatch is found late: the NFA sees the end last, and a chain finds its tail from the end only after its head. Adding an end-anchored test at the matcher entry rejects mismatched tails in `O(|suffix(P)|)` without entering the engine at all, which is the win on walker workloads dominated by paths that share a prefix with the pattern's literal head but differ at the extension.

The implementation accordingly stores only suffix facts.

## 4. Separator-aware comparison

The end-anchored comparison must respect the platform separator set `Sep` (`references/spec/GLOB_SPEC.md` §14.2): on Windows, a pattern suffix `/` should match a path-side `\`. The `ends_with` in `accept` (Rust `SegmentMatcher::ends_with`, JS `endsWithSepAware`) therefore matches a byte `b` against the suffix byte `s` as

```
b matches s   ⇔   (s ∈ Sep ⇒ b ∈ Sep) ∧
                  (s ∉ Sep ⇒ b == s under case-folding flag)
```

Without this rule, `*/main.ts` (suffix `/main.ts`) would incorrectly reject `src\main.ts` on Windows.

## 5. Extraction algorithm

`trailing(nodes)` (JS `trailingLiteral`) walks the pattern's node sequence right to left, collecting bytes:

```
Literal(b₁ … bₖ)             → push bₖ, …, b₁
Separator                    → push '/', unless it is the `/` of a `**/`
any other                    → halt collection
```

The collected bytes are reversed to form the suffix. Halting at the first other node is required for soundness: a wildcard or a `**` can produce arbitrary bytes at the suffix, so prepending its contribution would be unsound. The `/` right after a `**` (or after a brace a `**` ends) belongs to that `**/`, which absorbs zero segments too, so the suffix of `a/**/b` is `b`, not `/b`, and `**/b` keeps matching `b`.

`suffix_set(nodes)` (JS `extractSuffixSet`) is invoked when `trailing(nodes)` yields the empty suffix and the pattern ends with a brace of at most `MAX_SUFFIX_SET = 16` branches:

```
common_tail := trailing(nodes without the trailing brace)
for branch in branches:
    branch_suffix := trailing(branch)
    if branch is not all-literal AND branch_suffix is empty:
        return {}                    // abort the strategy
    full_suffix := if branch is all-literal then
                       common_tail · branch_suffix
                   else
                       branch_suffix
    if full_suffix is empty: return {}
    push full_suffix
```

The all-literal check is required because gluing `common_tail` to a non-all-literal branch's suffix would over-extend the suffix beyond what the branch guarantees.

## 6. Correctness invariant

**Proposition 3.** For every `w ∈ Σ*`,

```
accept(w) = 0   ⇒   w ∉ L(P).
```

_Proof sketch._ Both extraction routines yield sets `S` such that every `w ∈ L(P)` satisfies `∃ s ∈ S. w ends with s` (with `S` a singleton in the suffix case). `accept(w)` returns `1` iff `w` ends with at least one element of `S` under the separator-aware comparison. If `accept(w) = 0`, then `w` ends with none of them, so `w ∉ L(P)`. ∎

The corollary is that the matcher may safely short-circuit `is_match` to `false` whenever `accept(w) = 0`, whichever path of the exact engine would have run.

`accept` is **not** used by `match_dir`: a directory path in general does not yet end with the pattern's suffix (the pattern's literal tail occurs in the file segment, not the directory segment), so the prefilter would over-reject. `match_dir` therefore runs the engine without the prefilter.

## 7. Worked example

### 7.1 `**/*.ts` (single suffix)

```
AST              : [ Globstar, Separator, Star, Literal(".ts") ]
right-to-left:
  Literal(".ts") → push 's', 't', '.'
  Star           → halt (not a literal)
reverse buffer   → ".ts"
suffixes         = [ b".ts" ]               (suffix_set not tried: the suffix is non-empty)
```

`accept(path)`:

| `path`                | tail bytes vs `.ts`     | result | reason                               |
| --------------------- | ----------------------- | ------ | ------------------------------------ |
| `src/main.ts`         | `t`, `s`, `.` matches ✓ | true   | byte-equal                           |
| `src/main.tsx`        | `x`, `s`, `t` ≠ `.`     | false  | last byte mismatch                   |
| `src/main.js`         | `s`, `j`, `.` ≠ `t`     | false  | middle byte mismatch                 |
| `src\main.ts`         | matches ✓               | true   | only literal bytes, no `/` in suffix |
| (case `.TS`, ci=true) | accepted via fold       | true   | case-insensitive byte equal          |

The engine is invoked only on rows that returned `true`. For a walker pulling 10,000 paths, perhaps 200 are `.ts` files; 9,800 are rejected by `accept` for the cost of three byte compares each.

### 7.2 `**/*.{ts,tsx,js}` (suffix set)

```
AST              : [ Globstar, Separator, Star, Literal("."),
                     Brace([
                       Literal("ts"),
                       Literal("tsx"),
                       Literal("js"),
                     ]) ]

trailing(all nodes): walks right-to-left;
  the trailing brace halts the walk → suffix = ε

suffix_set:
  the last node is a brace of 3 (≤ 16) all-literal branches:
    common_tail = trailing([Globstar, Separator, Star, Literal(".")])
                = ".";
    branch[0] all-literal "ts"  → full = "." + "ts"  = ".ts"
    branch[1] all-literal "tsx" → full = "." + "tsx" = ".tsx"
    branch[2] all-literal "js"  → full = "." + "js"  = ".js"
suffix_set       = [ b".ts", b".tsx", b".js" ]
```

`accept(path)` walks the set and returns `true` iff `path` ends with at least one element. For `src/cli/run.tsx` the second element matches; for `Cargo.toml` none match.

### 7.3 `src/**/*` (no usable suffix)

```
AST              : [ Literal("src"), Separator, Globstar, Separator, Star ]
right-to-left:
  Star           → halt (not a literal)
suffix           = ε
suffix_set: the last node is not a brace → returns ∅
suffixes         = []
```

No anchor: `suffixes` is empty. `accept(path)` always returns `true`; the prefilter is bypassed and every candidate goes straight to the engine. This is acceptable: patterns without a literal tail genuinely cannot be filtered by an end-anchored test, and the engine itself runs at constant per-byte cost.

### 7.4 `src/main.{ts,rs}` (mixed-suffix set)

```
AST              : [ Literal("src"), Separator, Literal("main."),
                     Brace([ Literal("ts"), Literal("rs") ]) ]

trailing: the tail Brace halts the run → the nodes before it
suffix_set:
  common_tail = push_run(trailing([Lit("src"), Sep, Lit("main.")]))
              = "src/main."   (a Sep is collected as '/', so the run
                               goes on through it to Lit("src"))
  branch[0] → "src/main." + "ts" = "src/main.ts"
  branch[1] → "src/main." + "rs" = "src/main.rs"
suffix_set       = [ b"src/main.ts", b"src/main.rs" ]
```

`accept("src/main.ts")` succeeds on the first element; `accept("src/Cargo.toml")` fails both. `accept("x/src/main.rs")` passes, and the engine then rejects it: the prefilter may over-accept (§1).

The 11-byte anchor is more selective than the 3-byte ".ts" / ".tsx" / ".js" of §7.2 — long literal tails reject more paths per call, and the cost grows only linearly with the anchor length.

## 8. Source map

Rust, `crates/globstar/src/engine/segment/`:

- `facts.rs::suffixes(body)` — entry; returns the suffix list (`Box<[Bytes]>`), empty when there is no prefilter. Runs `trailing`, falls back to `suffix_set` if the single suffix is empty.
- `facts.rs::trailing` / `facts.rs::suffix_set` — internal helpers; `MAX_SUFFIX_SET = 16`.
- `segment/mod.rs::SegmentMatcher::prefilter(path)` — separator-aware end-anchored comparison (single suffix or set), the first step of `is_match`.

JS, `packages/globstar/src/engine/segment/`:

- `facts.js::suffixes(body)` — entry; returns a string array (one char per byte), empty when there is no prefilter.
- `facts.js::trailingLiteral` / `facts.js::extractSuffixSet` — internal helpers; `MAX_SUFFIX_SET = 16`.
- `facts.js::endsWithSepAware` — the separator-aware `ends_with`.
- `index.js::SegmentMatcher._factsAccept(str)` — the check, the first step of `_isMatch`.

## 9. References

The technique is sometimes called "literal acceleration" in the regex literature; the formal underpinning is the filter-then-verify schema:

- Boyer, R. and Moore, J. (1977). A Fast String Searching Algorithm. _CACM_ 20(10):762–772. The algorithmic ancestor of modern `memmem`.
- Wang, X. et al. (2019). Hyperscan: A Fast Multi-pattern Regex Matcher for Modern CPUs. _NSDI_. The most aggressive deployment of filter-then-verify in production.
- Aho, A. V. and Corasick, M. J. (1975). Efficient string matching: An aid to bibliographic search. _CACM_ 18(6):333–340. Multi- pattern alphabet for filter sets, applicable should we ship a multi-pattern accelerator over the `suffixes` lists in the future.
