# 05 — Literal Suffix Prefilter

The matcher composes a fast prefilter with the exact engine (`ThompsonDfa` or `PikeVm`). The prefilter rejects byte strings that cannot match the pattern by checking a guaranteed literal suffix; it is sound (no false negatives) and approximate (some false positives).

Implementation: `impl/crates/globstar/src/engine/facts.rs`.

## 0. Pipeline

```
Vec<Op>  ──►  extract_suffix(ops)        ──►  suffix:    Vec<u8>
   │          (right-to-left over Lit/Sep ops)        │
   │                                                  │
   └────►  if suffix is empty AND tail is Alternation:
              extract_suffix_set(ops)    ──►  suffix_set: Vec<Vec<u8>>

                                                  │
                                                  ▼
                                  LiteralFacts { suffix, suffix_set, ci }
                                                  │
                                                  │  carried inside the matcher
                                                  ▼
   ┌──────────────────────────────────────────────────────┐
   │ is_match(path):                                      │
   │   if !facts.accept(path) { return false }            │  ← O(|suffix|), end-anchored
   │   else { run engine }                                │
   └──────────────────────────────────────────────────────┘
```

`accept(path)` is a separator-aware `ends_with`: a `'/'` byte in the suffix matches any `Seps` byte in the path (so `**/main.ts` rejects `src\foo.js` on Windows and accepts `src\main.ts`).

When `suffix` is non-empty it is the single anchor. When the program ends in an `Op::Alternation` of all-literal branches (e.g. `**/*.{ts,tsx,js}`), `extract_suffix_set` instead derives one anchor per branch and `accept` succeeds if `path` ends with any of them.

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
suffix(P) := the longest s ∈ Σ* such that
             every w ∈ L(P) ends with s
```

(`s` may be `ε` if no non-empty common suffix exists). Then

```
prefilter_P(w) := w ends with suffix(P)
```

is a sound prefilter: any `w ∈ L(P)` ends with `suffix(P)` by definition. The implementation stores `suffix(P)` as `Vec<u8>` and evaluates `prefilter_P` by separator-aware `ends_with` (§4).

For patterns whose tail is an `Op::Alternation` of literal-only branches, we extract a _suffix set_ instead:

```
suffix_set(P) := { s_i : s_i is the suffix contributed by branch i }
prefilter_P(w) := ∃ s ∈ suffix_set(P). w ends with s
```

This handles common idioms like `**/*.{ts,tsx,js}` whose single longest common suffix is the `.` shared across branches but whose suffix-set is `{".ts", ".tsx", ".js"}`, providing a stronger filter.

## 3. Why suffix and not prefix

The exact engine is left-to-right. A prefix mismatch is detected on the first conflicting byte: the DFA transitions to `DEAD` and the byte loop exits. The cost is identical to a separate prefix-byte comparison.

A suffix mismatch is not detected until the engine has consumed the full input, because the engine has no native notion of an end anchor. Adding an end-anchored test at the matcher entry rejects mismatched tails in `O(|suffix(P)|)` without entering the engine at all, which is the win on walker workloads dominated by paths that share a prefix with the pattern's literal head but differ at the extension.

The implementation accordingly stores only suffix facts. The historical prefix facts were removed once the equivalence with the engine's natural prefix rejection was confirmed by measurement.

## 4. Separator-aware comparison

The end-anchored comparison must respect the platform separator set `Sep` (`references/spec/GLOB_SPEC.md` §12.3): on Windows, a pattern suffix `/` should match a path-side `\`. The `ends_with` in `LiteralFacts::accept` therefore matches a byte `b` against the suffix byte `s` as

```
b matches s   ⇔   (s ∈ Sep ⇒ b ∈ Sep) ∧
                  (s ∉ Sep ⇒ b == s under case-folding flag)
```

Without this rule, `**/main.ts` would incorrectly reject `src\main.ts` on Windows.

## 5. Extraction algorithm

`extract_suffix(ops)` walks the linear `Op` program right to left, collecting bytes:

```
Op::Lit(b₁ … bₖ)             → push bₖ, …, b₁
Op::Sep                      → push '/'
Op::SepRun, Op::OptSegmentsSlash → push '/' (one boundary byte)
any other                    → halt collection
```

The collected bytes are reversed to form the suffix. Halting at the first non-literal op is required for soundness: a non-literal op can produce arbitrary bytes at the suffix, so prepending its contribution would be unsound.

`extract_suffix_set(ops)` is invoked when `extract_suffix(ops)` yields the empty suffix and the program ends with `Op::Alternation(branches)`:

```
common_tail := extract_suffix(ops without the trailing Alternation)
for branch in branches:
    branch_suffix := extract_suffix(branch)
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
LiteralFacts::accept(w) = 0   ⇒   w ∉ L(P).
```

_Proof sketch._ Both extraction routines yield sets `S` such that every `w ∈ L(P)` satisfies `∃ s ∈ S. w ends with s` (with `S` a singleton in the suffix case). `accept(w)` returns `1` iff `w` ends with at least one element of `S` under the separator-aware comparison. If `accept(w) = 0`, then `w` ends with none of them, so `w ∉ L(P)`. ∎

The corollary is that the matcher may safely short-circuit `is_match` to `false` whenever `accept(w) = 0`, and the corollary holds independently of whether the exact engine is the DFA or the Pike VM.

`accept` is **not** used by `match_dir`: a directory path in general does not yet end with the pattern's suffix (the pattern's literal tail occurs in the file segment, not the directory segment), so the prefilter would over-reject. `match_dir` therefore runs the engine without the prefilter.

## 7. Worked example

### 7.1 `**/*.ts` (single suffix)

```
Vec<Op>          : [ OptSegmentsSlash, Star, Lit(".ts") ]
right-to-left:
  Lit(".ts")     → push 's', 't', '.'
  Star           → halt (non-literal)
reverse buffer   → ".ts"
suffix           = b".ts"
suffix_set       = []                       (suffix is non-empty)
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
Vec<Op>          : [ OptSegmentsSlash, Star, Lit("."),
                     Alternation([
                       [Lit("ts")],
                       [Lit("tsx")],
                       [Lit("js")],
                     ]) ]

extract_suffix(full ops): walks right-to-left;
  the trailing Alternation halts the walk → suffix = ε

extract_suffix_set:
  trailing op is Alternation, all-literal branches:
    common_tail = extract_suffix([OptSegmentsSlash, Star, Lit(".")])
                = ".";
    branch[0] all-literal "ts"  → full = "." + "ts"  = ".ts"
    branch[1] all-literal "tsx" → full = "." + "tsx" = ".tsx"
    branch[2] all-literal "js"  → full = "." + "js"  = ".js"
suffix_set       = [ b".ts", b".tsx", b".js" ]
```

`accept(path)` walks the set and returns `true` iff `path` ends with at least one element. For `src/cli/run.tsx` the second element matches; for `Cargo.toml` none match.

### 7.3 `src/**/*` (no usable suffix)

```
Vec<Op>          : [ Lit("src"), Sep, OptSegmentsSlash, Star ]
right-to-left:
  Star           → halt (non-literal)
suffix           = ε
extract_suffix_set: tail is not an Alternation → returns ∅
suffix_set       = []
```

Both anchors empty. `accept(path)` always returns `true`; the prefilter is bypassed and every candidate goes straight to the engine. This is acceptable: patterns without a literal tail genuinely cannot be filtered by an end-anchored test, and the engine itself runs at constant per-byte cost.

### 7.4 `src/main.{ts,rs}` (mixed-suffix set)

```
Vec<Op>          : [ Lit("src"), Sep, Lit("main."),
                     Alternation([ [Lit("ts")], [Lit("rs")] ]) ]

extract_suffix: tail Alternation halts → ε
extract_suffix_set:
  common_tail = "main." (from extract_suffix of the prefix)
                — actually walking right-to-left: the alternation
                  is the rightmost op, so common_tail is the suffix
                  of [Lit("src"), Sep, Lit("main.")] = "main."
  branch[0] → "main." + "ts" = "main.ts"
  branch[1] → "main." + "rs" = "main.rs"
suffix_set       = [ b"main.ts", b"main.rs" ]
```

`accept("src/lib/main.ts")` succeeds on the first element; `accept("src/Cargo.toml")` fails both.

The 7-byte anchor is more selective than the 3-byte ".ts" / ".tsx" / ".js" of §7.2 — long literal tails reject more paths per call, and the cost grows only linearly with the anchor length.

## 8. Source map

`engine/facts.rs`:

- `pub struct LiteralFacts { suffix, suffix_set, case_insensitive }`.
- `LiteralFacts::extract(ops, ci)` — entry; runs `extract_suffix`, falls back to `extract_suffix_set` if the single suffix is empty.
- `LiteralFacts::accept(path)` — separator-aware end-anchored comparison (single suffix or set).
- `extract_suffix(ops)` / `extract_suffix_set(ops)` — internal helpers.

## 9. References

The technique is sometimes called "literal acceleration" in the regex literature; the formal underpinning is the filter-then-verify schema:

- Boyer, R. and Moore, J. (1977). A Fast String Searching Algorithm. _CACM_ 20(10):762–772. The algorithmic ancestor of modern `memmem`.
- Wang, X. et al. (2019). Hyperscan: A Fast Multi-pattern Regex Matcher for Modern CPUs. _NSDI_. The most aggressive deployment of filter-then-verify in production.
- Aho, A. V. and Corasick, M. J. (1975). Efficient string matching: An aid to bibliographic search. _CACM_ 18(6):333–340. Multi- pattern alphabet for filter sets, applicable should we ship a multi-pattern accelerator over `LiteralFacts` derivatives in the future.
