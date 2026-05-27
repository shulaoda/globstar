# 03 — Subset Construction DFA

The Tier-1 matcher is a deterministic finite automaton constructed eagerly from the Thompson NFA (§02) by the powerset construction of Rabin and Scott (1959). This note formalizes the construction as implemented in `engine/thompson_dfa.rs`, with two implementation refinements: a byte-class equivalence reduction and a dual dedup representation for the subset states.

Implementation: `impl/crates/globstar/src/engine/thompson_dfa.rs`.

## 0. Pipeline

```
Thompson NFA  ─────►  build_byte_classes  ────►  class_of_byte: [u8; 256]
                                                     │
                                                     ▼
                          ┌─────────────┐    ┌──────────────────────┐
                          │ Builder BFS │───►│ transitions: Box<[u16]>│
                          │  (subset    │    │ accepting:   Box<[bool]>│
                          │   ctor)     │    └──────────────────────┘
                          └─────────────┘             │
                                 │                    ▼
                                 │           reverse-BFS
                                 │                    │
                                 ▼                    ▼
                          MAX_DFA_STATES?     reach_to_accept: Box<[bool]>
                                 │
                                 ├─ ≤ 4096 ─→  ThompsonDfa { ... }
                                 │
                                 └─ > 4096 ─→  Err(OpProgram) ─→ PikeVm (§04)
```

The build is split into four passes:

1. **`build_byte_classes`** signs each byte with a per-NFA-state behavior bitmask and partitions Σ into equivalence classes, producing `class_of_byte: [u8; 256]`.
2. **Builder BFS** enumerates reachable DFA states (subsets of `Q_N`) via the powerset construction over `Σ/≡`, filling `transitions` and `accepting`.
3. **Reverse BFS from `F_D`** yields `reach_to_accept`, used by `match_dir` to classify directory paths (§7).
4. If the BFS overruns the `MAX_DFA_STATES` budget, the program is handed back to the caller for a Pike VM compile.

The runtime hot path then degenerates to one indexed load per input byte:

```
state = transitions[(state << stride_shift) | class_of_byte[byte]]
```

## 1. The powerset construction

Given an NFA `N = (Q_N, Σ, δ_N, q₀, F_N)`, the powerset construction produces a DFA `D = (Q_D, Σ, δ_D, S₀, F_D)` with

```
Q_D ⊆ 2^{Q_N}
S₀  = ε-closure({q₀})
δ_D(S, a) = ε-closure({ q'  :  q ∈ S ∧ (q, a, q') ∈ δ_N })
F_D = { S ∈ Q_D  :  S ∩ F_N ≠ ∅ }
```

`Q_D` is finite: it is a subset of `2^{Q_N}`, hence `|Q_D| ≤ 2^{|Q_N|}`.

The construction proceeds by breadth-first enumeration of reachable subsets starting from `S₀` and computing `δ_D(S, a)` for every input symbol `a`. A new subset is added to `Q_D` whenever a transition yields a previously unseen subset.

## 2. The byte-class equivalence reduction

Iterating over all 256 bytes at every state is wasteful: most NFA states distinguish only a small number of byte classes (e.g. "separator" vs "ASCII letter `a`" vs "any other"). Define an equivalence on Σ:

```
b₁ ≡ b₂   ⇔   ∀ q ∈ Q_N. q transitions identically on b₁ and b₂
```

Let `[b]` denote the equivalence class of byte `b`, and `Σ/≡` the quotient alphabet. The DFA built over `Σ/≡` is bisimulation- equivalent to the DFA built over `Σ`; the transition table shrinks proportionally.

### 2.1 Computation

`build_byte_classes(thompson)` derives the equivalence in linear time over `Q_N` by signing each byte with a per-NFA-state acceptance bitmask and grouping bytes with identical signatures. The output is a table

```
class_of_byte: [u8; 256]
```

mapping each byte to its class index. Typical glob patterns yield 4–32 classes. The number of classes determines the row width of the DFA's transition table (`stride = nextPow2(num_classes)`).

### 2.2 The hot-loop encoding

The `stride` is rounded up to a power of 2 and stored as a shift amount, so the per-byte transition becomes one indexed load:

```rust
state = transitions[(state as usize << stride_shift)
                    | class_of_byte[byte as usize]];
```

The choice of shift over multiply was empirical (`engine/thompson_dfa.rs` documents the IMUL-vs-shift comparison inline). The choice keeps the hot loop branch-free and friendly to the prefetcher.

## 3. The state cap

`MAX_DFA_STATES = 4096` bounds `|Q_D|` during construction. When the construction would exceed the cap, `ThompsonDfa::build` returns `Err(program)`, surrendering ownership of the original program to the caller for a fallback Pike VM compile (§04).

The choice of cap balances three concerns:

- transition table size (`4096 · stride · 2` bytes; ≤ 256 KiB at `stride = 32`, fits in L2);
- compile latency (powerset construction is `O(|Q_D| · |Σ/≡|)` in state additions, plus dedup work);
- pattern coverage (under the dialect specified in `references/spec/GLOB_SPEC.md`, more than 99% of inputs we measured stay below 4096 states).

The cap is observable: a pattern's compile path is determined by its NFA shape, not by the data it is applied to. This is the property that motivates excluding bash extglobs (§01 §4).

## 4. Subset dedup: fast and wide paths

The construction must answer "have we seen this subset before?" at every transition. We split this on `|Q_N|`:

### 4.1 Fast path: `|Q_N| ≤ 64`

A subset can be represented in two `u64` words:

```
key = (lo: u64, hi: u64, at_segment_start: bool)
```

with `lo[i] = 1` iff state `i` is in the subset (and similarly for `hi[i] = 1` for `i ∈ [64, 128)`). Equality is two `u64` compares. Hashing is FxHash on the three components.

The dedup table is open-addressed, with `u32` slot ids and inline `(lo, hi, flag)` payloads. No heap allocation per transition.

### 4.2 Wide path: `|Q_N| > 64`

The subset is stored as a sorted `Vec<u32>` of NFA state ids and hashed via `FxHashMap` keyed on a serialized byte representation. Each transition allocates one Vec; the hashmap's amortization is the standard `O(1)` average.

Both paths share the outer BFS in `Builder::build`. The `use_fast_dedup` flag is set at construction time from `thompson.states.len() ≤ 64`.

## 5. The segment-start flag

`DotGuard` (§02 §2.7) requires that a DFA state's identity track the segment-start flag. The flag is part of the dedup key for both paths:

- fast path: a third boolean field in `(lo, hi, flag)`;
- wide path: a separate `bool` field included in the FxHashMap key.

The flag flips when a transition is taken on a separator byte. Two DFA states differing only in the flag are distinct.

## 6. The reach-to-accept mask

For every DFA state `S`, define

```
reach_to_accept(S) := ∃ a non-empty word w ∈ Σ* such that
                       δ_D*(S, w) ∈ F_D
```

i.e. `S` reaches an accepting state by some non-empty input. The mask is computed at the end of `ThompsonDfa::build` by reverse BFS from `F_D`.

The mask is consumed by `match_dir` (§06) and is not used during `is_match`. Pre-computation costs `O(|Q_D| + |edges|)`.

## 7. `is_match` and `match_dir`

`is_match(w)`:

```
state ← S₀
for b in w:
    state ← transitions[(state << shift) | class_of_byte[b]]
    if state == DEAD: return false
return accepting[state]
```

`DEAD` is the absorbing state (`δ_D(DEAD, a) = DEAD` for all `a`, `accepting[DEAD] = false`); it is encoded as state id 0 so the explicit `if state == DEAD` is a pruning optimization rather than a correctness requirement.

`match_dir(d)` runs `is_match` to produce a final state `s`, then takes one further transition on the implicit segment terminator:

```
exact ← accepting[s]
s_after_sep ← transitions[(s << shift) | class_of_byte[Sep]]
prefix ← accepting[s_after_sep] ∨ reach_to_accept[s_after_sep]
verdict ← match (exact, prefix) with
            | (true,  true)  → DescendAndMatch
            | (true,  false) → Match
            | (false, true)  → Descend
            | (false, false) → Pruned
```

The lookahead step is required because file systems return directory paths without the trailing separator (`references/spec/ GLOB_SPEC.md` §13, `references/spec/WALKER_SPEC.md` §6); the implicit `/` lookahead supplies the boundary the matcher's `OptSegmentsSlash` and `SlashAnything` ops need to enter their next segment.

## 8. Worked example: building the DFA for `a/*.ts`

This continues the running example from §02 §6. The NFA had 9 states; we now subset-construct its DFA.

### 8.1 Byte-equivalence classes

The NFA's byte-consuming states are: `q₀` (matches `'a'`), `q₁` (matches `'/'`), `q₄` (any non-sep, dot-protected), `q₅` (`'.'`), `q₆` (`'t'`), `q₇` (`'s'`).

Signing each byte by the set of NFA states that accept it:

| Byte (or class)        | `q₀` | `q₁` | `q₄` | `q₅` | `q₆` | `q₇` | Class id |
| ---------------------- | ---- | ---- | ---- | ---- | ---- | ---- | -------- |
| `a`                    | ✓    |      | ✓    |      |      |      | 0        |
| `t`                    |      |      | ✓    |      | ✓    |      | 1        |
| `s`                    |      |      | ✓    |      |      | ✓    | 2        |
| `.`                    |      |      | ✓ †  | ✓    |      |      | 3        |
| `/`                    |      | ✓    |      |      |      |      | 4        |
| any other non-sep byte |      |      | ✓    |      |      |      | 5        |
| any other separator    |      | ✓    |      |      |      |      | 4        |

† the `.` is also accepted by `q₄` when not at segment-start; the dot protection is a per-byte conditional handled at run time, not a separate class.

`num_classes = 6`. `stride = nextPow2(6) = 8`, `stride_shift = 3`. `class_of_byte['a'] = 0`, `class_of_byte['t'] = 1`, etc.

### 8.2 DFA states (subsets of NFA states)

The Builder BFS walks reachable subsets:

```
S₀ = ε-closure({q₀}) = {q₀}                          [DFA state 1, segment-start=true]
   ├─ class 0 ('a') → {q₁}                            [DFA state 2, segment-start=false]
   └─ everything else → DEAD                          [DFA state 0]

{q₁}
   ├─ class 4 ('/') → {q₂} → ε-close → {q₃, q₄, q₅}  [DFA state 3, segment-start=true]
   └─ everything else → DEAD

{q₃, q₄, q₅} (segment-start=true; q₄ has dot_protected active)
   ├─ class 0 ('a')   → q₄ accepts → {q₃,q₄,q₅}     [DFA state 4, segment-start=false]
   ├─ class 1 ('t')   → q₄ accepts → {q₃,q₄,q₅}     same as state 4
   ├─ class 2 ('s')   → q₄ accepts → {q₃,q₄,q₅}     same as state 4
   ├─ class 3 ('.')   → q₄ DROPPED (dot guard, segment-start=true);
   │                    q₅ accepts → {q₆}            [DFA state 5]
   ├─ class 4 ('/')   → DEAD (no NFA state in the set takes '/')
   └─ class 5 (other) → q₄ accepts → {q₃,q₄,q₅}     same as state 4

{q₃, q₄, q₅} (segment-start=false)                    [DFA state 4]
   ├─ class 0..2,5 ('a'/'t'/'s'/other) → {q₃,q₄,q₅}  self
   ├─ class 3 ('.')   → q₄ accepts (no guard now) → {q₃,q₄,q₅}
   │                    q₅ also accepts → {q₆}      union: {q₃,q₄,q₅,q₆}  [DFA state 6]
   └─ class 4 ('/')   → DEAD

{q₆}                                                  [DFA state 5]
   ├─ class 1 ('t')   → {q₇}                         [DFA state 7]
   └─ everything else → DEAD

{q₃, q₄, q₅, q₆}                                     [DFA state 6]
   ├─ class 0,2,3,5 ('a'/'s'/'.'/other) → {q₃,q₄,q₅,q₆} self
   ├─ class 1 ('t')   → q₄ → {q₃,q₄,q₅}; q₆ → {q₇}; union {q₃,q₄,q₅,q₇}  [DFA state 8]
   └─ class 4 ('/')   → DEAD

{q₇}                                                  [DFA state 7]
   ├─ class 2 ('s')   → {accept}                     [DFA state 9, accepting]
   └─ everything else → DEAD

{q₃, q₄, q₅, q₇}                                     [DFA state 8]
   ├─ class 2 ('s')   → q₄ → {q₃,q₄,q₅}; q₇ → {accept}; union {q₃,q₄,q₅,accept}  [DFA state 10, accepting]
   └─ ... (other classes loop back into the body, similar to state 4/6)

{accept-containing states 9, 10, ...}                 accepting, may loop on the body if `**`-style ops are present
```

The BFS terminates with ~10 DFA states for this small pattern, well under the 4096 cap. `accepting` is `true` for any DFA state whose subset contains `accept` (here, states 9 and 10).

### 8.3 The transition table

Row layout (`stride = 8`, `stride_shift = 3`):

```
              class 0  class 1  class 2  class 3  class 4  class 5
              ('a')    ('t')    ('s')    ('.')    ('/')    (other)
state 0 (DEAD)  →0        0        0        0        0        0
state 1 ({q₀}) →2        0        0        0        0        0
state 2 ({q₁}) →0        0        0        0        3        0
state 3 ({q₃,q₄,q₅}, seg-start) →4   4    4    5    0    4
state 4 ({q₃,q₄,q₅}, !seg-start)→4   4    4    6    0    4
state 5 ({q₆}) →0        7        0        0        0        0
state 6 ({...,q₆}) →6   8        6        6        0        6
...
```

`transitions[i << 3 | c]` gives the next DFA state. Width of each row is 8 (`1 << stride_shift`); slots beyond `num_classes = 6` are unused but kept for power-of-2 indexing.

### 8.4 Matching `"a/x.ts"`

```
state ← 1 (initial, S₀ = {q₀})
'a' (class 0) → transitions[1<<3 | 0] = state 2
'/' (class 4) → transitions[2<<3 | 4] = state 3       (segment-start flips to true)
'x' (class 5) → transitions[3<<3 | 5] = state 4       (segment-start back to false)
'.' (class 3) → transitions[4<<3 | 3] = state 6
't' (class 1) → transitions[6<<3 | 1] = state 8
's' (class 2) → transitions[8<<3 | 2] = state 10      (accepting)
end of input. accepting[10] = true → is_match returns true.
```

Six table lookups, no branches inside the loop, no NFA-state set materialized at run time.

### 8.5 The `reach_to_accept` mask

Reverse BFS from accepting states {9, 10}:

```
9, 10        accepting; reach_to_accept = true
8            ─'s'→ 10, ─other→ 8           reach_to_accept = true
7            ─'s'→ 9                       reach_to_accept = true
6            ─'t'→ 8                       reach_to_accept = true
5            ─'t'→ 7                       reach_to_accept = true
4            ─'.'→ 6                       reach_to_accept = true
3            ─'.'→ 5                       reach_to_accept = true
2            ─'/'→ 3                       reach_to_accept = true
1            ─'a'→ 2                       reach_to_accept = true
0 (DEAD)     loops to itself                 reach_to_accept = false
```

For `match_dir("src", P)` where `P = a/*.ts`, the run lands in DEAD on the first byte ('s' has class 2, transitions[1<<3|2] = 0). `accepting[0] = false`, `reach_to_accept[0] = false` ⇒ verdict is `Pruned`. The walker may safely skip the entire `src/` subtree.

For `match_dir("a", P)`: run lands in state 2 after 'a'. `s_after_sep = transitions[2<<3 | 4 ('/')] = 3`. `accepting[2] = false` (exact = false). `accepting[3] = false` and `reach_to_accept[3] = true` (prefix = true) ⇒ verdict is `Descend`. The walker recurses into `a/`.

## 9. Source map

`engine/thompson_dfa.rs`:

- `pub const MAX_DFA_STATES: usize = 4096`.
- `pub struct ThompsonDfa { transitions, class_of_byte, stride_shift, accepting, initial, reach_to_accept, facts, prefixes }`.
- `ThompsonDfa::build(program, dot) -> Result<Box<Self>, OpProgram>`.
- `build_byte_classes(thompson) -> ([u8; 256], num_classes, tracks_seg_start, has_dot_guard)`.
- `Builder` (private) drives the BFS and selects the dedup path.

## 10. References

- Rabin, M. O. and Scott, D. (1959). Finite automata and their decision problems. _IBM J. Res. Dev._ 3(2):114–125.
- Hopcroft, J. E., Motwani, R., and Ullman, J. D. _Introduction to Automata Theory, Languages, and Computation_, ch. 4.
- Thompson, K. (1968). Regular Expression Search Algorithm. _CACM_ 11(6):419–422.
