# Theory Notes — Index

These notes record the formal foundations of the engine modules under `impl/crates/globstar/src/engine/`. Their scope is the algorithms actually compiled into this implementation; algorithms surveyed during exploration but not adopted (Glushkov index, Brzozowski derivatives, bit-parallel simulation, multi-pattern Aho-Corasick) are out of scope for this directory.

## Reading order

1. [01-glob-as-regular-language.md](01-glob-as-regular-language.md) — the glob dialect specified in `references/spec/GLOB_SPEC.md` is a regular language under the closure operations of union, concatenation, and Kleene star. The leading `!` and the absence of `!(...)` extglobs are discussed.
2. [02-thompson-nfa.md](02-thompson-nfa.md) — Thompson's NFA construction; the seven `Trans` variants used by the implementation; the segment-start dot rule realized as a `DotGuard` state.
3. [03-subset-construction-dfa.md](03-subset-construction-dfa.md) — subset construction over the Thompson NFA; byte-class equivalence reduction; the fast / wide dedup paths; the bounded state count.
4. [04-pike-vm.md](04-pike-vm.md) — linear-time NFA simulation used when subset construction exceeds its state cap.
5. [05-literal-prefilter.md](05-literal-prefilter.md) — the suffix- anchored literal prefilter used to short-circuit the matcher; its correctness invariant.
6. [06-walker-and-pruning.md](06-walker-and-pruning.md) — static- prefix extraction and the four-valued `match_dir` predicate, which together drive directory-level pruning.
7. [07-segment-matcher.md](07-segment-matcher.md) — the segment-structured matcher (SSM), a standalone experimental engine (`crates/globstar-segment`, `packages/globstar-segment`): patterns as linear element sequences, anchored single-globstar matching, the element-position NFA behind `match_dir`, and the JS string/byte dual-mode execution.

## Engine map

| Source file              | Theory note | Role                                                                                                          |
| ------------------------ | ----------- | ------------------------------------------------------------------------------------------------------------- |
| `engine/literal.rs`      | —           | Tier 0 byte-equality matcher for pure-literal patterns.                                                       |
| `globstar-segment` crate | §07         | Segment-structured matcher — standalone experimental engine, benchmarked against this crate's tiers.          |
| `engine/ops/`            | §06         | IR, AST-to-linear lowering, normalization, and static-prefix analysis.                                        |
| `engine/facts.rs`        | §05         | Suffix prefilter.                                                                                             |
| `engine/thompson.rs`     | §02         | Thompson NFA.                                                                                                 |
| `engine/thompson_dfa.rs` | §03         | Subset-constructed DFA — primary Tier 1/2 engine.                                                             |
| `engine/pikevm.rs`       | §04         | NFA simulation — fallback and ReDoS soundness floor.                                                          |

## Notation

- `Σ` denotes the input alphabet (the set `{0, 1, ..., 255}` of bytes) unless stated otherwise.
- `Σ*` is the set of finite byte strings, including the empty string `ε`.
- An NFA is a 5-tuple `(Q, Σ, δ, q₀, F)`: states, alphabet, transition relation, initial state, accepting set.
- A DFA is a 5-tuple `(Q, Σ, δ, q₀, F)` with `δ: Q × Σ → Q` total.
- For a regular expression `r`, `L(r) ⊆ Σ*` is its language. `|r|` is its size (count of grammar symbols).

References to the implementation use the source-file path and the identifier name (`thompson_dfa.rs::ThompsonDfa::build`, etc.).
