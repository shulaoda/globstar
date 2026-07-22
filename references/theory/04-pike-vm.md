# 04 — NFA Simulation (Pike VM)

PikeVm is the total fallback for patterns that the bounded segment matcher cannot represent. It simulates the Thompson NFA from §02 without backtracking, so matching remains polynomial and cannot exhibit ReDoS-style exponential behavior.

Implementations:

- Rust: `crates/globstar/src/engine/pikevm.rs` over `engine/thompson.rs`.
- JavaScript: `packages/globstar/src/matcher/engine/pikevm.js` over `engine/nfa-soa.js`.

## 1. Dispatch and ownership

Production dispatch is:

```text
pure literal → LiteralMatcher
otherwise    → SegmentMatcher.build(program)
                 ├─ success → SegmentMatcher
                 └─ decline → PikeVm
```

PikeVm accepts every valid `OpProgram`; it has no pattern-shape or state-count rejection. The segment engine may decline when cross-segment expansion, sequence count, or in-segment state budgets are exceeded. The unchanged program is then compiled once into the Thompson representation.

## 2. Active-set semantics

Let `Q` be the NFA states and `C(S)` the ε-closure of a state set through `Split` and `Jump`. PikeVm maintains an active set `S ⊆ Q`:

```text
S₀ = C({q₀})
Sᵢ₊₁ = ⋃ { C({next(q)}) : q ∈ Sᵢ and q accepts byte bᵢ }
```

After the final byte, the path matches iff `S` intersects `accepts_at_eof`. `DotGuard` is not part of the static ε-closure because its edge depends on whether the upcoming byte is a segment-leading `.`; the guarded run path expands it conditionally.

This computes only the subset reached by the current input. It does not precompute a transition table for every reachable subset.

## 3. Bitmap representation

An active set is a bitmap with `ceil(|Q| / word_bits)` words. Each NFA state also has a precomputed closure bitmap. A byte step therefore:

1. iterates set bits in the current bitmap;
2. tests the corresponding byte-consuming state;
3. ORs the successor's closure bitmap into the next bitmap;
4. swaps current and next.

There are two loops:

- fast path: no `DotGuard`, one pass over the active bits per byte;
- guarded path: a `processed` bitmap deduplicates conditional ε-work introduced by passing `DotGuard` states.

The literal suffix facts from §05 run before `is_match`, but not before `match_dir`, because a directory prefix need not yet contain the pattern's suffix.

## 4. Runtime-specific layout

Rust uses `u64` bitmaps. Up to 256 NFA states use one stack buffer containing current, next, processed, and (for `match_dir`) after-separator slots. Larger NFAs allocate one contiguous `Vec<u64>` per call. The compiled matcher has no interior mutability and remains `Send + Sync`.

JavaScript uses `Uint32Array`. Compile-time SoA arrays are packed into one runtime array containing closure bitmaps, initial/accept bitmaps, and one metadata word per state. Scratch storage is retained by the matcher and reused because JavaScript execution is single-threaded. Character-class objects remain in a sparse side array.

The layouts differ to suit each runtime, while their state tags, closure rules, and transition semantics remain aligned.

## 5. Directory pruning

`match_dir(d)` first runs `d` without the suffix prefilter:

```text
exact = active(d) intersects accepts_at_eof
after_sep = step(active(d), '/')
prefix = after_sep intersects accepts_at_eof or reach_to_accept
return DirMatch::from_exact_prefix(exact, prefix)
```

`reach_to_accept[q]` means that some non-empty byte sequence can reach the accepting state from `q`. Rust packs this mask eagerly during construction. JavaScript computes it lazily on the first directory query so ordinary `isMatch`-only matchers do not pay for it.

Before the hypothetical separator step, any live `DotGuard` is expanded to a fixpoint: `/` is never a segment-leading dot, so those guards necessarily pass.

## 6. Complexity

For input length `n`, NFA state count `m`, active state count `a`, and bitmap word count `w = ceil(m / word_bits)`:

- construction: Thompson NFA plus closure-table computation;
- match space: `O(m)` active/scratch bits, excluding the precomputed closure table;
- byte step: at most `O(a · w)` bitmap work;
- total match time: polynomial and bounded by `O(n · m² / word_bits)` for the packed implementation.

For common fallback patterns, `w` is small and the loop behaves like `O(n · a)`. The important architectural property is totality: every valid program has a non-backtracking execution path even when the faster bounded engine declines it.

## 7. Source map

| Responsibility        | Rust                                    | JavaScript                         |
| --------------------- | --------------------------------------- | ---------------------------------- |
| Thompson construction | `engine/thompson.rs::Thompson::compile` | `engine/nfa-soa.js::compileNfaSoa` |
| Static ε-closures     | `thompson.rs::compute_static_closures`  | `pikevm.js::staticClosuresN`       |
| Packed runtime        | `pikevm.rs::PikeVm`                     | `pikevm.js::PikeVm`                |
| Full match            | `PikeVm::is_match`                      | `PikeVm.isMatch`                   |
| Directory query       | `PikeVm::match_dir`                     | `PikeVm.matchDir`                  |

## 8. References

- Cox, R. (2009). _Regular Expression Matching: the Virtual Machine Approach_. https://swtch.com/~rsc/regexp/regexp2.html
- Cox, R. (2007). _Regular Expression Matching Can Be Simple And Fast_. https://swtch.com/~rsc/regexp/regexp1.html
- Thompson, K. (1968). _Regular Expression Search Algorithm_. CACM 11(6):419–422.
