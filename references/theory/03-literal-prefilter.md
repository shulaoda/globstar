# 03. The Literal Suffix Prefilter

The matcher composes a cheap test with the exact engine. The test rejects a path that cannot match the pattern by comparing its end with a literal suffix every match carries. It is sound (it rejects no match) and approximate (it accepts some non-matches). Source: `crates/globstar/src/engine/segment/literal_ends.rs` and `packages/globstar/src/engine/segment/literal-ends.js`.

## 1. Prefilters

**Definition.** A *prefilter* for a language `L ⊆ Σ*` is a predicate `A : Σ* → {0, 1}` such that

```
A(w) = 0   ⇒   w ∉ L
```

equivalently, `w ∈ L ⇒ A(w) = 1`. `A` may accept a `w ∉ L`. Its use is that `A` is cheaper than membership in `L`, so a matcher answers `false` on `A(w) = 0` without running the exact engine. The schema is known as *filter, then verify*; a common instance in text search replaces a regular-expression match with a literal search.

## 2. The suffix prefilter

For a pattern `P`, let

```
suffix(P) := a string s ∈ Σ* such that every w ∈ L(P) ends with s
```

`s` may be ε, and it need not be the longest such string: `{xa,ya}c` gives `c`. Then

```
A_P(w) := w ends with suffix(P)
```

is a sound prefilter, since every `w ∈ L(P)` ends with `suffix(P)` by definition. The comparison is separator-aware (§4).

When `suffix(P)` is ε and `P` ends with a brace of at most 16 branches (`MAX_SUFFIX_SET`), a *suffix set* is used instead:

```
suffix_set(P) := { sᵢ : sᵢ is the suffix contributed by branch i }
A_P(w)        := ∃ s ∈ suffix_set(P).  w ends with s
```

This covers the idiom `**/*.{ts,tsx,js}`, whose common suffix is empty (`.ts` and `.tsx` end differently) and whose suffix set is `{".ts", ".tsx", ".js"}`. A brace of more than 16 branches gets no prefilter; its literal branches are grouped by the engine itself (note 02, §7.1), and a brace with a wildcard branch is matched by the segment's NFA.

## 3. Extraction

`trailing(nodes)` reads the pattern's node sequence right to left and collects bytes:

```
Literal(b₁ … bₖ)   →  collect bₖ, …, b₁
Separator          →  collect '/', unless it is the `/` of a `**/`
any other node     →  stop
```

The collected bytes, reversed, are the suffix. Stopping at the first other node is required for soundness: a wildcard or a `**` can produce any bytes, so including a contribution from it would be unsound. The `/` after a `**` (or after a brace a `**` ends) belongs to that `**/`, which absorbs zero segments as well, so the suffix of `a/**/b` is `b` and not `/b`, and `**/b` keeps matching `b`.

`suffix_set(nodes)` (JS `suffixSet`) runs when `trailing` yields ε and the last node is a brace of at most 16 branches:

```
common := trailing(nodes without the last brace)
for each branch:
    s := trailing(branch)
    if the branch is not all literal and s = ε:  return ∅
    full := common · s   if the branch is all literal
            s            otherwise
    if full = ε:  return ∅
    add full
```

The all-literal condition is required: appending `common` to the suffix of a branch that holds a wildcard would claim more than the branch guarantees.

## 4. Separator-aware comparison

The comparison respects the separator set `Seps` (GLOB_SPEC §14.2): on Windows a pattern suffix `/` must match a path byte `\`. A path byte `b` matches a suffix byte `s` as

```
b matches s   ⇔   b ∈ Seps                              if s = '/'
                  false                                 if s ∈ Seps and s ≠ '/'   (an escaped `\` on Windows)
                  b = s, case-folded under the option    otherwise
```

Rust: `bytes::ends_with_pattern`; JS: `endsWithPattern`. Without the first rule, `*/main.ts`, whose suffix is `/main.ts`, would reject `src\main.ts` on Windows.

## 5. Soundness

**Proposition 2.** For every pattern `P` and every `w ∈ Σ*`, `A_P(w) = 0 ⇒ w ∉ L(P)`.

*Proof.* Both extraction procedures yield a set `S` such that every `w ∈ L(P)` ends with some `s ∈ S` (a singleton in the first case). `A_P(w) = 1` exactly when `w` ends with some element of `S` under the comparison of §4, which accepts every byte a match may have there. If `A_P(w) = 0`, then `w` ends with no element of `S`, so `w ∉ L(P)`. ∎

The matcher may therefore answer `is_match(w) = false` whenever `A_P(w) = 0`, whichever path of the exact engine would have run.

The prefilter is not applied to `match_dir`. A directory path does not in general end with the pattern's suffix, which lies in the file segment, so the test would reject directories that have matches below them.

## 6. Suffix rather than prefix

The engine reads from the left: a chain matches its head first and the NFA reads left to right, so a prefix mismatch is found at the first differing segment or byte, and a prefix prefilter would repeat that work. A suffix mismatch is found last: the NFA sees the end last, and a chain finds its tail after its head. An end-anchored test at the matcher's entry rejects a mismatched tail in `O(|suffix(P)|)` without entering the engine. This is the common case on a walk, where most paths share the pattern's literal head and differ in the extension.

## 7. Examples

**`**/*.ts`.** Nodes: `Globstar, Separator, Star, Literal(".ts")`. `trailing` collects `s`, `t`, `.` and stops at the star: `suffix = ".ts"`.

| Path                | Result | Reason                                  |
| ------------------- | ------ | --------------------------------------- |
| `src/main.ts`       | 1      | ends with `.ts`                         |
| `src/main.tsx`      | 0      | last byte `x`                           |
| `src/main.js`       | 0      | third-last byte `j`                     |
| `src\main.ts`       | 1      | no `/` in the suffix                    |
| `MAIN.TS` (folded)  | 1      | under `case_insensitive`                |

The engine runs only on the paths the test accepts.

**`**/*.{ts,tsx,js}`.** `trailing` stops at the brace: `suffix = ε`. The brace has three literal branches; `common = trailing(Globstar, Separator, Star, Literal("."))` is `.`, and the set is `{".ts", ".tsx", ".js"}`.

**`src/**/*`.** `trailing` stops at the star and the last node is not a brace: no prefilter. Every path goes to the engine, which runs at constant cost per byte.

**`src/main.{ts,rs}`.** `trailing` stops at the brace; `common = "src/main."`, since a separator is collected as `/` and the run continues through it; the set is `{"src/main.ts", "src/main.rs"}`. `A("x/src/main.rs") = 1`, and the engine rejects the path: the prefilter may over-accept (§1).

## 8. Source map

| Symbol                                                 | File                                                |
| ------------------------------------------------------ | --------------------------------------------------- |
| `suffixes(body)`, `trailing`, `suffix_set`, `MAX_SUFFIX_SET` | `crates/globstar/src/engine/segment/literal_ends.rs` |
| `SegmentMatcher::prefilter`                            | `crates/globstar/src/engine/segment/mod.rs`         |
| `ends_with_pattern`                                    | `crates/globstar/src/bytes.rs`                      |
| `suffixes`, `trailing`, `suffixSet`                    | `packages/globstar/src/engine/segment/literal-ends.js` |
| `SegmentMatcher._prefilter`                            | `packages/globstar/src/engine/segment/index.js`     |
| `endsWithPattern`                                      | `packages/globstar/src/bytes.js`                    |

## 9. References

- Boyer, R. S. and Moore, J. S. (1977). A fast string searching algorithm. *Communications of the ACM* 20(10), 762–772.
- Aho, A. V. and Corasick, M. J. (1975). Efficient string matching: an aid to bibliographic search. *Communications of the ACM* 18(6), 333–340.
- Wang, X. et al. (2019). Hyperscan: a fast multi-pattern regex matcher for modern CPUs. *NSDI*.
