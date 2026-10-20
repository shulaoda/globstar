# Theory Notes — Index

These notes record the formal foundations of the engine modules under `crates/globstar/src/engine/`. They cover the algorithms in the current code only.

## Reading order

1. [01-glob-as-regular-language.md](01-glob-as-regular-language.md) — the glob dialect specified in `references/spec/GLOB_SPEC.md` is a regular language under the closure operations of union, concatenation, and Kleene star. The leading `!` and the absence of `!(...)` extglobs are discussed.
2. [02-segment-engine.md](02-segment-engine.md) — the segment engine, the one engine for every non-literal pattern: patterns as a graph of per-segment elements, anchored matching of linear paths, the NFA over the graph behind unions and `match_dir`, and the JS string/byte dual-mode execution.
3. [03-literal-prefilter.md](03-literal-prefilter.md) — the suffix-anchored literal prefilter used to short-circuit the matcher; its correctness invariant.
4. [04-walker-and-pruning.md](04-walker-and-pruning.md) — static-prefix extraction and the six-valued `match_dir` predicate, which together drive directory-level pruning.

## Engine map

| Source file               | Theory note | Role                                                       |
| ------------------------- | ----------- | ---------------------------------------------------------- |
| `engine/literal.rs`       | —           | Byte-equality matcher for pure-literal patterns.           |
| `engine/segment/`         | §02         | The segment engine, for every non-literal pattern.         |
| `engine/segment/facts.rs` | §03, §04    | Suffix prefilter (§03) and static-prefix analysis (§04).   |

## Notation

- `Σ` denotes the input alphabet (the set `{0, 1, ..., 255}` of bytes) unless stated otherwise.
- `Σ*` is the set of finite byte strings, including the empty string `ε`.
- An NFA is a 5-tuple `(Q, Σ, δ, q₀, F)`: states, alphabet, transition relation, initial state, accepting set.
- For a regular expression `r`, `L(r) ⊆ Σ*` is its language. `|r|` is its size (count of grammar symbols).

References to the implementation use the source-file path and identifier name (`segment/compile.rs::Graph::build`, etc.).
