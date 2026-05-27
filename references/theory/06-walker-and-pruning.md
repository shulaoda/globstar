# 06 — Static-prefix Extraction and `match_dir`

The matcher exposes two outputs that drive directory-level pruning:

- `static_prefixes(P)`, the deepest segment-bounded literal prefix per top-level brace branch of `P`;
- `match_dir(P, d)`, a four-valued predicate over directory paths classifying whether the pattern can match `d` itself, descendants of `d`, both, or neither.

Implementation:

- `engine/ops.rs::compute_static_prefixes` — the prefix extraction pass.
- `engine/thompson_dfa.rs::ThompsonDfa::match_dir` — DFA realization.
- `engine/pikevm.rs::PikeVm::match_dir` — Pike VM realization.
- `dir_match.rs` — the `DirMatch` algebraic datatype.

This note formalizes both and documents how the walker (`references/spec/WALKER_SPEC.md`) consumes them.

## 0. Pipeline

```
   pattern ──► matcher.compile ──► matcher  ──► static_prefixes()
                                          │
                                          └──► match_dir(d)
                                                    │
                                                    ▼
                                    DirMatch ∈ {Pruned, Descend, Match, DescendAndMatch}

Walker startup:
   for each prefix p in matcher.static_prefixes():
       seed = cwd ++ p
       fs.statSync(seed):
         missing      → silently drop this branch
         file & match → push as result
         directory    → push frame {absolute=seed, relative=p}

Walker per directory frame (absolute, relative):
   for each entry in fs.readdir(absolute):
       child_rel = relative ++ "/" ++ entry.name
       if ignore.match(child_rel):     skip
       else if entry is directory:
           dm = matcher.match_dir(child_rel)
           if DirMatch::should_descend(dm):
               enqueue child frame
           # Match flag is unused: walker emits files only
       else if matcher.match(child_rel):
           emit child path
```

Two outputs from the matcher drive the walker: `static_prefixes` jumps the traversal head past the literal prefix that every match must carry, and `match_dir` answers "could this directory's subtree contain any match?" once per discovered directory.

## 1. Static prefixes

### 1.1 Definition

For a pattern `P` whose lowered form is the linear `Op` program `o₁ o₂ … oₙ`, define the _static prefix_ of `P` as

```
SP(P) := the longest s ∈ Σ* such that
         every w ∈ L(P) starts with s,
         and s ends at a path-segment boundary
        (i.e. either s = ε or the last byte of s is in Sep).
```

The segment-boundary requirement matters because the walker uses `SP(P)` to seed traversal at `cwd ++ s`, and `fs.readdir` only operates on directories. A non-segment-aligned prefix would force the walker to filter mid-segment, which it cannot.

### 1.2 Extraction

`compute_static_prefixes(ops)` returns one prefix per top-level brace branch:

- if `ops[0] = Op::Alternation(branches)`, recurse into each branch and collect the union of returned prefixes;
- otherwise, scan `o₁, o₂, …` left to right, accumulating bytes from `Op::Lit` and separator bytes from `Op::Sep`/`Op::SepRun`; halt at the first non-literal op or at the first separator whose successor is non-literal.

The algorithm is `O(|ops|)`. Output is `Box<[Box<[u8]>]>`, cached on the matcher.

### 1.3 Examples

```
src/main.rs        →  ["src/main.rs"]    (fully literal)
src/*.ts           →  ["src"]
src/**/*.ts        →  ["src"]
**/*.ts            →  [""]               (no literal head)
{src,test}/*.rs    →  ["src", "test"]
```

### 1.4 Properties

**Proposition 4.** For every `w ∈ L(P)`, there exists `s ∈ SP(P)` such that `w` starts with `s`.

_Proof sketch._ Top-level alternations produce per-branch prefixes by construction. Each non-alternation prefix is built only from ops whose languages contain no Σ-quantifier in the head segment, so it is a guaranteed left-anchored byte string of every match in that branch. ∎

The corollary is that the walker may safely seed a separate traversal frame for each `s ∈ SP(P)`: paths that do not start with any `s` are excluded from `L(P)`, so they need not be visited.

## 2. The `DirMatch` predicate

### 2.1 Definition

```rust
pub enum DirMatch {
    Pruned,
    Descend,
    Match,
    DescendAndMatch,
}
```

Given `P`, a directory path `d`, and the canonicalized form `d' = d ++ "/"` (so `d'` is a path prefix in segment terms), the walker uses

```
exact_match(P, d) := d' ∈ L(P)
prefix_match(P, d) := ∃ w ∈ Σ*. d' ++ w ∈ L(P) ∧ w ≠ ε
match_dir(P, d) := combine(exact_match(P, d), prefix_match(P, d))
```

with

```
combine(true,  true)  = DescendAndMatch
combine(true,  false) = Match
combine(false, true)  = Descend
combine(false, false) = Pruned
```

`Pruned` is the only verdict that licenses the walker to skip `readdir(d)`.

### 2.2 DFA realization

The DFA's state at the end of `d` summarizes the active NFA subset. Append the implicit segment terminator and consult the reach-to-accept mask:

```
s₀ ← run d through ThompsonDfa to a final state
s₁ ← δ_D(s₀, Sep)
exact   ← accepting[s₀]
prefix  ← accepting[s₁] ∨ reach_to_accept[s₁]
verdict ← combine(exact, prefix)
```

`reach_to_accept` is the backward-BFS mask defined in §03 §6. The implicit `Sep` step is necessary because file-system directory paths typically arrive without trailing separator (`d` ≠ `d'` in the input the walker holds), but the matcher's `OptSegmentsSlash` and `SlashAnything` expect a `Sep` to enter their next segment.

### 2.3 Pike VM realization

Same predicate, lifted to the simulator's active set:

```
S₀ ← run d through PikeVm to a final active set
S₁ ← ε-closure({ q' : q ∈ S₀, (q, sep, q') ∈ δ_N })
exact   ← S₀ ∩ accepts_at_eof ≠ ∅
prefix  ← (S₁ ∩ accepts_at_eof ≠ ∅) ∨ (S₁ ∩ reach_to_accept ≠ ∅)
verdict ← combine(exact, prefix)
```

`reach_to_accept` is the bit vector computed eagerly inside `PikeVm::new` (§04 §4).

### 2.4 The suffix prefilter is not used

Unlike `is_match`, `match_dir` does not consult `LiteralFacts::accept` (§05). A directory prefix in general does not end with the pattern's suffix — the pattern's literal tail lives in the file segment, not the directory segment — so the prefilter would over-reject. Both engines run the byte loop unfiltered for `match_dir`.

### 2.5 Negated patterns

For multi-pattern matchers (§ `engine/glob.rs::compileMatcher` in the JS port), if any branch of `P` is negated, the verdict is forced conservative:

```
match_dir(P with negations, d) ∈ {Descend, DescendAndMatch}
```

i.e. pruning is suppressed. A negated branch matches paths _not_ in some sublanguage, and the walker has not yet seen the deeper paths; it cannot rule out their membership in the complement. The implementation still preserves the positive `Match` flag where possible: if the positive engine reports `Match`, the verdict is `DescendAndMatch`; otherwise it is `Descend`. Pruning is restored only when no negative branch exists.

This is the formal counterpart of `references/spec/GLOB_SPEC.md` §13.4. The walker (§ `references/spec/WALKER_SPEC.md` §4) splits `!`-prefixed input patterns into its own ignore set, so the matcher receives only positive patterns at the walker layer; the conservative case applies only to direct standalone callers.

## 3. Walker integration

The walker's responsibilities relative to this module are documented in `references/spec/WALKER_SPEC.md`. From the matcher's side:

- `static_prefixes()` is consumed once at construction time (`prepare()` → `initFromPrefixes`) to produce seed frames.
- `match_dir(child_rel)` is consulted once per discovered directory during `processDirents`. The return value gates the descend decision; the `Match` flag is unused because the walker is hard-wired to emit only file paths.
- `is_match(child_rel)` is consulted once per discovered file.
- `ignore.is_match(child_rel)` is consulted once per discovered entry of either kind, before any other matcher work.

## 4. Properties

**Proposition 5 (no false pruning).** For every `P`, `d`, and `w ∈ Σ*`:

```
match_dir(P, d) = Pruned   ⇒   ∀ w. d' ++ w ∉ L(P)
```

_Proof sketch._ `Pruned` is returned only when `exact = false ∧ prefix = false`. `exact = false` means `d' ∉ L(P)`. `prefix = false` means `accepting[s₁] = 0 ∧ reach_to_accept[s₁] = 0`, so no non-empty extension of `d'` reaches an accepting state. Together, no `d' ++ w` (with `w` empty or non-empty) is accepted. ∎

This is the soundness property the walker requires: it may safely prune any subtree whose root receives `Pruned`.

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

Pattern: `src/**/*.ts`. Compiled matcher has `static_prefixes = [b"src"]` (the literal head before the first non-literal op).

### 5.1 Seeding

```
matcher.static_prefixes() = [ b"src" ]

walker iterates:
  prefix = b"src"
  seed   = "/proj/src"
  statSync(seed) → directory
  matchDir.shouldDescend(matcher.match_dir(b"src")) = true
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
  → emit "src/main.ts"

For "components":
  child_rel = "src/components"
  isDirectory = true
  matcher.match_dir("src/components") = ?
    DFA run:
      state 1 ('s') → 2 → ('r') → ... → some state s
      s_after_sep   → reach_to_accept = true (deeper paths can still match)
      accepting[s]  = false
    verdict = Descend
  → enqueue frame { absolute: "/proj/src/components", relative: "src/components" }

For "cli": (similar to "components")
  matcher.match_dir("src/cli") = Descend
  → enqueue frame { absolute: "/proj/src/cli", relative: "src/cli" }
```

After this frame: `out = ["src/main.ts"]`, two new frames queued.

### 5.3 Second frame: `/proj/src/components`

```
fs.readdir(...) → [Button.tsx, Card.tsx]

For "Button.tsx":
  child_rel = "src/components/Button.tsx"
  isDirectory = false
  matcher.match("src/components/Button.tsx"):
    DFA reaches state where last bytes are "tsx" but the pattern's
    suffix is ".ts" → ends_with(".ts") fails on the prefilter (LiteralFacts.accept).
    Actually: the suffix is ".ts" and the path is "...x".
    accept(path) returns false (suffix mismatch on last byte 'x' vs 's').
  → match = false; do not emit

For "Card.tsx": same, do not emit.
```

### 5.4 Third frame: `/proj/src/cli`

```
fs.readdir(...) → [run.ts]

For "run.ts":
  child_rel = "src/cli/run.ts"
  isDirectory = false
  facts.accept("src/cli/run.ts") → ends with ".ts" → true
  matcher.match runs the DFA → reaches accept
  → emit "src/cli/run.ts"
```

Final result: `["src/main.ts", "src/cli/run.ts"]`.

The walker performed exactly 3 `readdir` calls (one per directory under `src/`). The competing naive walk would have done 4 + the lodash subtree (4000+ readdir calls). The savings come entirely from `static_prefixes` (skipping `/proj/tests` and `/proj/node_modules` upfront) and from the suffix prefilter inside `matcher.match` (rejecting `.tsx` files in `O(1)` per file instead of running the DFA).

### 5.5 With an ignore: `glob("src/**/*.ts", { ignore: ["**/*.test.ts"] })`

Same setup, plus `tests/foo.test.ts`. The walker compiles a second matcher (`ignore`) for `**/*.test.ts`. At each entry the walker calls `ignore.match(child_rel)` first; matching paths skip without recursion. For directories the walker uses `match` (not `match_dir`) on the ignore matcher because ignore patterns are evaluated on entry, not as subtree predicates.

```
For "tests/foo.test.ts":
  ignore.match("tests/foo.test.ts") = true  → skip without emit
For "src/cli/run.ts":
  ignore.match("src/cli/run.ts")    = false → continue (emitted via main matcher)
```

If `**/*.test.ts` were instead routed through `match_dir`, the walker could prune directories that contain only test files; we deliberately do not do this (Pruned via ignore would require proving every descendant matches the ignore, which is not generally decidable from a pattern's structure alone).

## 6. Source map

| Symbol                                                                              | File                                                                       |
| ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `compute_static_prefixes`                                                           | `engine/ops.rs`                                                            |
| `ThompsonDfa::match_dir`                                                            | `engine/thompson_dfa.rs`                                                   |
| `PikeVm::match_dir` / `has_prefix_descent`                                          | `engine/pikevm.rs`                                                         |
| `compute_reach_to_accept`                                                           | `engine/thompson.rs`                                                       |
| `DirMatch`, `DirMatch::is_match`, `DirMatch::should_descend`, `DirMatch::is_pruned` | `dir_match.rs`                                                             |
| Walker glue (JS)                                                                    | `packages/globstar/src/walker/walk.js::initFromPrefixes`, `processDirents` |

## 7. References

- Hopcroft, J. E., Motwani, R., Ullman, J. D. _Introduction to Automata Theory, Languages, and Computation_. Reverse reachability and quotient automata are textbook material; the `reach_to_accept` BFS is a direct application.
- The `DirMatch` four-valued formulation is characteristic of the Rust `globset` / `ignore` ecosystem; the equivalent verdict is used internally by `walkdir::WalkDir::filter`. The literature for prefix-based pruning of regular-expression search lives in index-pattern work (e.g. _Cox, R., Russ Cox's regular expression notes_) and incremental DFA matching.
