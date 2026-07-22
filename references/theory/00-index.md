# Theory Notes — Index

These notes record the formal foundations of the engine modules under `crates/globstar/src/engine/`. Their scope is the algorithms actually compiled into this implementation; algorithms surveyed during exploration but not adopted are out of scope.

## Reading order

1. [01-glob-as-regular-language.md](01-glob-as-regular-language.md) — the glob dialect specified in `references/spec/GLOB_SPEC.md` is a regular language under the closure operations of union, concatenation, and Kleene star. The leading `!` and the absence of `!(...)` extglobs are discussed.
2. [02-thompson-nfa.md](02-thompson-nfa.md) — Thompson's NFA construction; the seven `Trans` variants used by the implementation; the segment-start dot rule realized as a `DotGuard` state.
3. [07-segment-matcher.md](07-segment-matcher.md) — the primary segment-structured matcher (SSM): patterns as linear element sequences, anchored single-globstar matching, the element-position NFA behind `match_dir`, and the JS string/byte dual-mode execution.
4. [04-pike-vm.md](04-pike-vm.md) — total NFA simulation used when the segment engine cannot represent a pattern within its budgets.
5. [05-literal-prefilter.md](05-literal-prefilter.md) — the suffix-anchored literal prefilter used to short-circuit the matcher; its correctness invariant.
6. [06-walker-and-pruning.md](06-walker-and-pruning.md) — static-prefix extraction and the four-valued `match_dir` predicate, which together drive directory-level pruning.

## Engine map

| Source file          | Theory note | Role                                                                    |
| -------------------- | ----------- | ----------------------------------------------------------------------- |
| `engine/literal.rs`  | —           | Tier 0 byte-equality matcher for pure-literal patterns.                 |
| `engine/segment/`    | §07         | Primary segment-structured matcher for non-literal production patterns. |
| `engine/ops/`        | §06         | IR, AST-to-linear lowering, normalization, and static-prefix analysis.  |
| `engine/facts.rs`    | §05         | Suffix prefilter.                                                       |
| `engine/thompson.rs` | §02         | Thompson NFA builder used only by the fallback.                         |
| `engine/pikevm.rs`   | §04         | NFA simulation — fallback and ReDoS soundness floor.                    |

## Notation

- `Σ` denotes the input alphabet (the set `{0, 1, ..., 255}` of bytes) unless stated otherwise.
- `Σ*` is the set of finite byte strings, including the empty string `ε`.
- An NFA is a 5-tuple `(Q, Σ, δ, q₀, F)`: states, alphabet, transition relation, initial state, accepting set.
- For a regular expression `r`, `L(r) ⊆ Σ*` is its language. `|r|` is its size (count of grammar symbols).

References to the implementation use the source-file path and identifier name (`pikevm.rs::PikeVm::new`, etc.).
