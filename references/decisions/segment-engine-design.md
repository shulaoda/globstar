# SSM — Segment-Structured Matcher (design)

**Status:** implemented as the standalone `globstar-segment` crate and
`@globstar/segment` package (sharing the `globstar` parser/lowering),
benchmarked head-to-head against the `globstar` engines in
BENCHMARKS.md; adversarially reviewed against the byte
engines (~950 empirical probes, three review agents) and corrected —
see §10 for the errata the review produced. **Date:** 2026-07-22.

Goal: replace the default match engines (eager ThompsonDfa in Rust, PikeVm
in JS) with one algorithm — identical in both runtimes — that preserves
GLOB_SPEC v0.2.0 semantics byte-for-byte while winning all three axes
(compile time, memory, match throughput) against both the current engines
and the competitor libraries (globset/wax in Rust; picomatch/minimatch/
micromatch in JS). PikeVm remains the general fallback, so the ReDoS
guarantee (worst case O(n·m)) is unchanged.

## 1. Why segments

Every construct in the dialect except `**` is segment-local: `*`, `?`,
`[...]` cannot cross `/`, classes reject `Seps` members, dot-protection is
keyed on segment starts. `**` is the only construct that crosses
separators, and after the existing globstar fold it appears in exactly
four forms (`LeadingSeps+OSS`, `SepRun+OSS`, `SlashAnything`,
`GlobstarAny`). So a pattern is *naturally* a linear sequence of
segment-shaped elements, and matching is naturally segment-at-a-time —
where every per-segment check is a memcmp/ends_with-class primitive that
vectorizes, instead of a per-byte automaton step that serializes.

The compile side wins even harder: segmentation is a single linear pass
over the ops with no subset construction, no hash maps, no ε-closures.

## 2. The path model

A path `p` (bytes) is viewed as segments split on **every** `Seps` byte
(`/` always; `\` too on Windows). Splitting is total and lossless; empty
segments are real:

```
""      → [""]           "a"     → ["a"]
"/"     → ["", ""]       "a/"    → ["a", ""]
"a//b"  → ["a", "", "b"] "/a"    → ["", "a"]
```

The pattern side compiles to a sequence of **elements**:

```
Element := Lit(bytes)      -- literal segment, memcmp (sep-free by construction)
         | Wild(matcher)   -- in-segment matcher compiled from in-segment ops
         | G0              -- globstar absorbing ≥ 0 segments
         | G1              -- globstar absorbing ≥ 1 segment
```

`match(pattern, p)` ⇔ the element sequence derives the segment sequence,
where `Lit`/`Wild` each consume exactly one segment and `G0`/`G1` absorb a
run of segments (of any content, including empty segments), subject to the
dot rule (§5).

### 2.1 Mapping the folded ops to elements

Ops are scanned linearly, cutting at `Sep` boundaries:

| Op run                          | Elements                  | Notes |
|---------------------------------|---------------------------|-------|
| in-segment ops … `Sep` …        | `Lit`/`Wild` per segment  | consecutive `Sep`s ⇒ empty `Lit("")` between them |
| `LeadingSeps, OSS` (pattern head) | `G0`                    | absorbs leading empty segments (abs/UNC paths) and any further segments |
| `SepRun, OSS` (mid `/**/`)      | `G0`                      | SepRun's 1+ separator run = the boundary between the previous element and the absorbed run |
| `OSS` after alternation-head recursion | `G0`               | per-branch `LeadingSeps` handled identically inside forks |
| `SlashAnything` (trailing `/**`)| *close segment*, then `G1`| the pattern's leading `/` (if any) leaves an empty `Lit` first: `/**` = `[Lit "", G1]` — matches `/`, not `a` |
| `GlobstarAny` (bare `**`)       | `G0` at a fresh boundary; **`G1` behind a strict `Sep`** (`a/{**,x}` rejects `a`); **upgraded to `G1` when a strict `Sep` follows** (`{**,x}/b` rejects `b`) | brace-spliced `.*` composes with strict separators, which each demand ≥ 1 absorbed segment |
| adjacent `G` elements           | keep both — ε-skips chain them; never collapse across a strict `Sep` (each surviving `Sep` emits its `Lit ""`): `**//**` = `[G0, Lit "", G1]` | `[G, G]` runs from forks (`{**,x}/**`) are not single-G expressible and take the NFA path or fall back |

**Equivalence claims** (each verified against the byte engines by the
corpus + differential fuzzer; reviewers: attack these):

- **C1 (mid `**`)**: `A/**/B` ⇔ elements `[…A, G0, B…]`. The byte form is
  `A SepRun OSS B`; `SepRun` (1+ seps) followed by `OSS`
  (`(seg sep-run)*`) generates exactly: one separator, then any sequence
  of segments each followed by a separator run. In segment terms: the
  boundary `/` that separates `A` from what follows, then **any (possibly
  empty) sequence of segments** — empty ones arising from extra
  separators in the run — then `B`. So G0 = "absorb any k ≥ 0 segments".
  Examples: `a/**/b` matches `a/b` (k=0), `a//b` (k=1, [""]),
  `a/x//y/b` (k=3, [x,"",y]).
- **C2 (leading `**/`)**: `**/X` ⇔ `[G0, X…]`. Byte form
  `LeadingSeps OSS X`: 0+ leading seps (= leading empty segments) then
  `(seg sep-run)*`. Absorbs any k ≥ 0 segments including leading empty
  ones: `/a/X` = ["", a] absorbed; `//srv/share/X` = ["", "", srv, share].
- **C3 (trailing `/**`)**: `A/**` ⇔ `[…A, G1]`. Byte form
  `A SlashAnything` = `A / .*`: one required separator then anything.
  Segment terms: at least one more segment after `A` (the first may be
  empty: `a/` = [a, ""] matches; `a` = [a] does not), then arbitrary
  further content = arbitrary further segments.
- **C4 (bare `**`)**: `**` ⇔ `[G0]` where G0 consumes the whole segment
  list — matches everything (dot rule aside), including `""` → [""]
  (one empty segment absorbed… note [""] must be absorbable by G0 for
  `**` to match the empty path; G0 absorbing "the entire list" vs the
  fixed-point "empty path has one empty segment" is resolved in §3:
  a sequence of elements matches the segment LIST, and `[G0]` matches
  [""] by absorbing the single empty segment).
- **C5 (strict Sep)**: `a/b` ⇔ `[Lit a, Lit b]` and `a//b` ⇔
  `[Lit a, Lit "", Lit b]` — segment-count equality gives the strict
  behavior for free (`a/b` vs path `a//b`: 2 ≠ 3 elements → no match).
- **C6 (trailing `/`)**: `a/` ⇔ `[Lit a, Lit ""]` — matches exactly
  `a/` (and `a\` on Windows), not `a`.

### 2.2 Forks (separator-crossing braces)

An `Op::Alternation` whose branches contain `Sep`/globstar ops cannot stay
inside one segment. The segmentizer forks the element sequence: each
branch is segmentized in context (concatenated with the surrounding
prefix/suffix ops), producing `k ≤ FORK_CAP` alternative element
sequences. `{a,b/c}x` → sequences `[Lit "ax"]` and `[Lit "b", Lit "cx"]`.
Nested sep-crossing braces multiply; when the product exceeds `FORK_CAP`
(default 64 sequences) the pattern falls back to PikeVm. Purely
in-segment braces (`*.{ts,tsx}`) do NOT fork — they stay inside one
`Wild` matcher as alternation, preserving §7.2's no-cartesian-expansion
guarantee for the common case. (Forking IS a bounded expansion; the cap +
fallback keeps compile/memory linear. Note `{a,b}{c,d}{e,f}…` stays
in-segment = one Wild, no expansion at all.)

Match = OR over sequences; match_dir = flag-combine (§6).

## 3. Matching

Let elements `E[0..m]`, path segments `S[0..n]` (n ≥ 1 always — even ""
has one segment).

- **No G** (fixed depth): match ⇔ n == m ∧ ∀i: E[i] consumes S[i].
- **Single G at index g** (the dominant real-world case): anchored two-end
  match, zero search:
  1. head: ∀i < g: E[i] consumes S[i] (requires n ≥ g)
  2. tail: t = m − g − 1 tail elements; ∀j < t:
     E[g+1+j] consumes S[n−t+j] (requires n − g ≥ t, i.e. the middle
     `mid = S[g .. n−t]` has length ≥ 0)
  3. G0: |mid| ≥ 0; G1: |mid| ≥ 1
  4. dot rule on `mid` (§5)
- **Multiple Gs**: segment-level greedy with backtracking — the classic
  wildcard-match two-pointer where the "alphabet" is segments and `G`
  plays `*`. O(n·m) worst case (same bound as PikeVm), linear in
  practice. Head before the first G and tail after the last G are still
  anchored; only inter-G element runs search.

The suffix `LiteralFacts` prefilter is kept unchanged in front of
everything (it rejects most candidates in walker workloads for ~1-4ns).

### 3.1 In-segment matchers (`Wild`)

In-segment op runs (over `Lit/AnyChar/Star/Class/Alternation`, all
sep-free) are classified at compile time:

| Kind        | Shape                | Match primitive |
|-------------|----------------------|-----------------|
| `WAny`      | `*`                  | dot-check only |
| `WSuffix`   | `*lit`               | len ≥ ‖lit‖ ∧ ends_with(lit) ∧ dot-check |
| `WPrefix`   | `lit*`               | starts_with(lit) |
| `WPreSuf`   | `lit1*lit2`          | len ≥ ‖lit1‖+‖lit2‖ ∧ starts_with ∧ ends_with |
| `WSuffixSet`| `*.{a,b,…}` (all-literal trailing alternation) | any-of ends_with ∧ dot-check |
| `WGeneric`  | anything else        | u64 bitmask position-NFA over in-segment ops (linear time, built without hash maps; > 64 positions → whole-pattern PikeVm fallback) |

Dot-check (§5) applies to any wildcard-led matcher. Case-insensitive mode
routes every byte compare through the ASCII fold (compile-time expanded
classes stay as today).

`?` handling: runs of `?` translate to min-length increments; `?*` ⇒
`WAny` with min_len 1; `???` ⇒ exact len 3 (`WGeneric` fast-cased as
`WLen{min,max}` when the segment is only `?`s/`*`s: len bounds + dot).

## 4. Precise dot rule (dot=false)

A segment is **dot-led** iff it is nonempty and its first byte is `.`.
Under `dot=false`:

- `G0`/`G1` may not absorb a dot-led segment (empty segments are fine —
  they have no first byte). This mirrors the byte engines' AnyByte/
  AnyNonSep `dot_protected` gates at segment starts, including position 0.
- A wildcard-led `Wild` matcher rejects dot-led segments outright — this
  encodes the DotGuard "star dies even on zero-match" rule (`*.txt`
  vs `.txt` → no match).
- A `Wild` whose first op is a literal `.` (or a class containing `.`,
  positive) matches dot-led segments normally — literals are never
  protected. Classes: positive class with explicit `.` member → allowed;
  negated class → protected (may not consume the leading dot).
- `Lit` segments are unaffected (byte equality).

Head/tail anchoring makes the mid-segment dot scan a single memchr-style
sweep: reject if any `p[i] == '.'` with `i == mid_start ∨ p[i−1] ∈ Seps`
inside the mid byte range.

## 5. match_dir (element-position NFA)

`match_dir(d)` = simulate the element sequence over d's segments with an
active-position bitmask (positions 0..=m over a ≤64-element sequence fits
u64; larger → PikeVm fallback — element counts above 64 are absurd
patterns). Transition on segment s: position e with `E[e] ∈ {Lit,Wild}`
advances to e+1 if it consumes s; position at a G absorbs s (stay, dot
rule permitting) or was already advanced past it via the G's ε-skip
(G0's "absorb zero" = position e also seeds e+1 at closure time; G1
seeds e+1 only after absorbing ≥ 1).

After consuming all of d's segments:

- `exact` = accept-closure bit set (position m, or m−1 when E[m−1] = G0).
- `prefix` (could `d/…` match?) = simulate one more "fresh segment
  boundary" step: from the active set, is there a position that can
  consume ≥ 1 further segment and reach accept? Precomputed per-position
  suffix-satisfiability masks make this a couple of ANDs. Satisfiability
  accounts for degenerate never-matching `Wild`s (e.g. `[^\x00-\xFF]`-ish
  classes) and the dot rule ONLY where it is unconditional (a wildcard-led
  tail element with dot=false is still satisfiable — by a non-dot-led
  descendant — so it stays satisfiable; a G blocked by a dot-led ABSORBED
  segment is a per-run check, which the simulation already performed).
- Combine as today: `DirMatch::from_exact_prefix(exact, prefix)`.

This is exactly the byte engines' "run + hypothetical `/` step +
reach-to-accept" logic, lifted to segment granularity. It is used for
ALL SSM patterns (single-G fast paths for match_dir are a later
optimization; correctness first).

`static_prefixes` falls out structurally: leading `Lit` elements up to
the first non-Lit, per fork branch, deduped — same output as today's
`compute_static_prefixes`.

## 6. Multi-pattern union

`Glob::union` compiles each pattern independently (Literal / SSM /
PikeVm), then buckets:

- **suffix bucket**: patterns of shape `[G0, Wild∈{WSuffix,WSuffixSet}]`
  (`**/*.ts`, `**/*.{ts,tsx}`) merge into one suffix-set probe (linear
  scan of a few suffixes; ext-keyed map if the bucket grows).
- **first-segment literal map**: patterns with a leading `Lit` bucket by
  that literal's bytes; match extracts S[0] once and probes.
- **residual list**: everything else, linear with per-pattern prefilters.

`is_match` = bucket probes, short-circuit. `matches`-style index queries
push candidate indices. `match_dir` aggregates over all patterns
(buckets don't apply to dirs) with the existing early-exit combine; each
per-pattern match_dir is a cheap u64 simulation. The old
factor/probe/merge path and `NFA_FAST_PATH_LIMIT` decomposition are
retired for SSM-eligible patterns.

Compile becomes O(Σ pattern bytes) with tiny constants — no probe
Thompson, no merged subset construction, no re-parses.

## 7. JS specifics

Same algorithm, same element structures. Two execution modes:

- **String mode** (default): match directly on the JS string —
  `charCodeAt`/`startsWith`/`endsWith`-style primitives, zero per-call
  allocation, no `toBytes`. Correctness vs byte semantics:
  - Literal segment compares, `WPrefix/WSuffix/WPreSuf` are safe for ANY
    input: string equality ⇔ UTF-8 byte equality on the same Unicode
    text; separators and `.` are ASCII and never appear inside a
    multi-unit char's UTF-8 encoding or surrogate pair.
  - `*`/G absorption is safe: runs of non-sep chars ⇔ runs of non-sep
    bytes; dot-led checks inspect ASCII `.` only.
  - The ONLY divergent constructs are `?` (one *byte* per spec, one
    UTF-16 unit in string mode) and **negated classes** (match one byte).
    These bail: when such a matcher meets a char code > 0x7F at its
    position, the whole call re-runs in byte mode (rare; correct).
  - Patterns containing non-ASCII bytes in classes, or any non-ASCII
    anywhere for simplicity, compile to byte mode only.
- **Byte mode**: `toBytes(input)` once, same matchers over `Uint8Array`.
  Also used when the caller passes a `Uint8Array`.

The walker's `__engine: "dfa"` opt-in is replaced by SSM (it serves
match/matchDir/staticPrefixes natively). PikeVm remains the JS fallback
for the same shapes as Rust.

## 8. Memory layout

Rust: one flat allocation per compiled SSM — element table (fixed-width
records with offset/len into a shared byte blob) + blob + suffix facts.
Target ≤ 300 B for every bench single-pattern row. JS: small monomorphic
objects, shared shapes; no typed-array tables except `WGeneric` masks.

## 9. Fallback + testing

Dispatch (in the standalone crate/package): Literal → LiteralMatcher
(shared); segmentizable ∧ elements ≤ 64 ∧ forks ≤ 64 ∧ WGeneric
positions ≤ 64 → SSM; else the shared PikeVm — the only *total*
engine, and the O(n·m) ReDoS floor. The eager ThompsonDfa is NOT in
the fallback chain: it would re-import the subset-construction
compile cost for a tier that still could not drop the PikeVm (the
DFA has its own state cap). It remains the `globstar` crate's primary
engine and the corpus oracle.

Gates, in order, after every change: `cargo test --workspace`,
`node verify.mjs` (corpus, all engines incl. SSM rows added),
`node fuzz.mjs` sweeps (JS-SSM ↔ Rust-SSM cross-runtime; plus Rust
SSM ↔ PikeVm and JS SSM ↔ PikeVm in-process oracles), then
`node bench.mjs`.

## 10. Adversarial-review errata (all folded into §2.1 and the code)

Three review agents probed the draft's element mapping against the
byte engines (difftest + the JS engines, ~950 tuples). Findings:

1. **`/**` and `/**/a`** — a pattern-leading `/` before a globstar
   fold emits an empty `Lit` element first (`[Lit "", G1]`,
   `[Lit "", G0, Lit a]`); the draft's table lost the
   absolute-path requirement.
2. **Brace-spliced bare `**` composes strictly** — `{**,x}/y`,
   `x/{**,q}`, `a/{**,q}/b` all behave as `G1`, not `G0`: a strict
   separator adjacent to a spliced `.*` demands ≥ 1 absorbed segment.
   Only the native folds (`SepRun`/`LeadingSeps`/`OSS`/
   `SlashAnything`) absorb their boundary separators.
3. **No G-collapse across separators** — `**//**` ≡
   `[G0, Lit "", G1]` (each extra `/` between globstar folds is a
   strict `Sep` emitting `Lit ""`); `{**,x}/**` ≡ `[G1, G1]`
   (minimums ADD — absorb ≥ 2 segments), which the single-G fast
   path cannot express (multi-G NFA or automata fallback).
4. **Escaped separators** — `\/` lowers into a `Lit` containing a
   real separator byte, matched byte-exactly by the automata tiers.
   Segments are sep-free, so such patterns are not
   segment-expressible: fall back (`a\/b*`).
5. **`match_dir` does NOT canonicalize** `d` to `d + "/"` — the
   engines simulate exactly d's segments (`a/` vs dir `a` →
   Descend, not Match; spec §13.2's formula is stale, engines win).
   `prefix` = ∃ continuation of ≥ 1 segments (empty allowed,
   multiple allowed) reaching accept — realized as the per-state
   `reach1` mask over suffix satisfiability.
6. **Spec drift found (not SSM bugs)**: GLOB_SPEC §8.4 says
   `match_dir("a/", "a/**") = Match` but both engines return
   DescendAndMatch; §13.4's negated-pattern guidance is followed by
   JS but not Rust (pre-existing runtime divergence, out of scope
   here — the fuzzer strips `!` for `d` ops).
7. **Pre-existing bug found by the review tooling**: the JS PikeVm
   `match_dir` DotGuard ε-fixpoint compared a signed `|` result
   against an unsigned `Uint32Array` read — an infinite loop
   whenever bit 31 participated (≥ 32-state NFA + `dot=false`).
   Fixed with `>>> 0` in `pikevm.js`.
