# Theory Notes

These notes give the formal foundations of the implementation under `crates/globstar/src/engine/` and its JS twin under `packages/globstar/src/engine/`. The specifications in `references/spec/` state what the implementations must do; the notes state how they do it and why the methods are correct. They describe the current code only.

## Reading order

1. [01-glob-as-regular-language.md](01-glob-as-regular-language.md). The language of GLOB_SPEC.md is regular. The note gives the translation of each construct into the closure operations, the treatment of the leading `!` as predicate inversion, and the reason extglob negation is excluded.
2. [02-segment-engine.md](02-segment-engine.md). The engine: a pattern as a graph of per-segment elements, the layers that match it, the thresholds that choose between them, their measured cost, and the JS execution over strings.
3. [03-literal-prefilter.md](03-literal-prefilter.md). The suffix prefilter that rejects a path before the engine runs, and its soundness.
4. [04-walker-and-pruning.md](04-walker-and-pruning.md). Static prefixes and the directory predicate `match_dir`, which together let a walker skip directories, and the soundness of both.

## Source map

| Source                           | Note    | Role                                                  |
| -------------------------------- | ------- | ----------------------------------------------------- |
| `engine/literal.rs`              |         | byte-equality matcher for pure-literal patterns       |
| `engine/segment/`                | 02      | the segment engine, for every other pattern           |
| `engine/segment/literal_ends.rs` | 03, 04  | the suffix prefilter and the static prefixes          |

## Notation

- Σ is the alphabet, the set of bytes `{0, …, 255}`. Σ* is the set of finite byte strings and ε the empty string.
- `u · v` is the concatenation of `u` and `v`; `L₁ · L₂ = { u · v : u ∈ L₁, v ∈ L₂ }`.
- For a pattern `P`, `L(P) ⊆ Σ*` is its language as GLOB_SPEC §2.4 defines it.
- An NFA is a tuple `(Q, Σ, δ, q₀, F)`: states, alphabet, transition relation, initial state, accepting states.
- `Seps ⊆ Σ` is the separator set of GLOB_SPEC §2.2.

Source code is cited by file path and identifier, as `segment/graph.rs::Graph::build`.
