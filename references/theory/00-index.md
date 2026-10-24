# Theory Notes

These notes describe the implementation under `crates/globstar/src/`, `crates/globstar-walk/src/` and their JS twins under `packages/globstar/src/` and `packages/globstar-walk/src/`. The specifications in `references/spec/` state what the implementations must do; the notes state how each component does it, why the method is correct, and what it costs. One note per component, in the order a pattern passes through them.

## Reading order

1. [01-parser-and-unions.md](01-parser-and-unions.md). The parser, the AST, the choice of engine, and how the members of a union are factored into one pattern.
2. [02-literal-engine.md](02-literal-engine.md). The engine for a pattern without wildcards.
3. [03-segment-engine.md](03-segment-engine.md). The engine for every other pattern: the graph of segment elements, the chains, the chain index, the NFAs, the in-segment matchers, the suffix prefilter, the static prefixes and the directory predicate.
4. [04-walker.md](04-walker.md). The two walkers: preprocessing, seeding, the per-entry rule, scheduling, errors and symbolic links.

## Source map

| Source                                          | Note | Role                                               |
| ----------------------------------------------- | ---- | -------------------------------------------------- |
| `globstar/src/parse/`, `union.rs`, `lib.rs`     | 01   | parsing, factoring, engine selection               |
| `globstar/src/engine/literal.rs`                | 02   | the literal engine                                 |
| `globstar/src/engine/segment/`                  | 03   | the segment engine                                 |
| `globstar/src/dir_match.rs`                     | 03   | the six directory results                          |
| `globstar-walk/src/`, `globstar-walk/src/walk.js` | 04 | the walkers                                        |

Each Rust file has a JS twin of the same name under `packages/`; the notes name the Rust file and mention the JS one where the two differ.

## Notation

- Σ is the alphabet, the set of bytes `{0, …, 255}`. Σ* is the set of finite byte strings and ε the empty string.
- `u · v` is the concatenation of `u` and `v`.
- For a pattern `P`, `L(P) ⊆ Σ*` is its language as GLOB_SPEC §2.4 defines it.
- `Seps ⊆ Σ` is the separator set of GLOB_SPEC §2.2.

Source code is cited by file path and identifier, as `segment/graph.rs::Graph::build`.
