# 04 — NFA Simulation (Pike VM)

The Tier-1 fallback matcher simulates the Thompson NFA (§02) directly, following Cox (2009). It is invoked when `ThompsonDfa::build` (§03) returns `Err(program)` because the powerset construction would exceed `MAX_DFA_STATES`. The simulator runs in `O(n · m)` time where `n` is the input length and `m = |Q_N|`.

Implementation: `impl/crates/globstar/src/engine/pikevm.rs`.

## 0. Pipeline

```
Thompson NFA  ─────►  PikeVm::new
                          │
                          ▼   PikeVm { thompson, facts, prefixes, reach_to_accept }
   ┌───────────────────────────────────┐
   │ Per call (is_match / match_dir):  │
   │                                   │
   │  Scratch { current, next,          │   (ε-closure scratch buffers)
   │            visited: u32[],         │
   │            generation: u32 }        │
   │                                   │
   │  for each input byte b:           │
   │    next ← ∅                       │
   │    bump generation                │
   │    for each q in current:         │
   │      step(states[q], b, next, ...)│
   │    swap(current, next)            │
   └───────────────────────────────────┘
```

Pike VM never materializes a DFA. The `current` set is the running powerset state (§03 §1) computed on the fly per byte. `visited[q] = generation` says "state `q` was added to `next` in this byte's step", which dedupes ε-paths in `O(1)` per insert — bumping the generation invalidates every entry without zeroing the array.

## 1. The simulator

Maintain two state sets `S_curr, S_next ⊆ Q_N`. Initially:

```
S_curr ← ε-closure({q₀})
```

For each input byte `b`, expand:

```
S_next ← ε-closure({ q' :
    q ∈ S_curr,
    (q, b, q') ∈ δ_N,
    DotGuard predicate satisfied for q on b
})
```

then swap `S_curr ↔ S_next`. After consuming the entire input, `is_match(w)` ≡ `S_curr ∩ accepts_at_eof ≠ ∅`.

This is a direct realization of the powerset semantics from §03 without materializing the DFA: at each byte, the active state set is computed on the fly. The work per byte is bounded by `|S_curr|` state expansions plus their ε-closure, which is `O(m²)` in the worst case and `O(m)` in practice with bitset closures.

## 2. ε-closure with deduplication

The textbook `ε-closure(S)` may visit the same state multiple times along distinct ε-paths. Without deduplication, the closure can re-enter exponentially. The standard fix maintains a per-step `visited: array of bool` table reset between bytes.

The implementation uses a generation-stamped variant:

```
struct Scratch {
    current:    Vec<StateId>,
    next:       Vec<StateId>,
    visited:    Vec<u32>,
    generation: u32,
}
```

The `visited[s] = generation` invariant means "state `s` was added in the current step". Bumping `generation` between steps invalidates every entry in `O(1)`, replacing the per-step `O(m)` `fill(0)`. The trick is folklore in modern regex engines; for our use-case the amortized improvement matters when patterns produce hundreds of NFA states.

## 3. `DotGuard` evaluation

`Trans::DotGuard { next }` (§02 §2.7) is a state whose successor edge is conditional on the upcoming byte. The simulator handles it during the byte-step rather than during ε-closure:

```
when processing byte b at state s = DotGuard { next }:
    if at_segment_start ∧ b == b'.':
        thread dies         (ε-edge not taken)
    else:
        recursively process states[next] on b
```

ε-closure admits `DotGuard` to the active set but does not follow the outgoing edge; the byte-step §1 therefore observes `DotGuard` states and dispatches accordingly.

## 4. The reach-to-accept mask

For `match_dir` Pike VM computes

```
reach_to_accept[s] := ∃ a non-empty byte sequence taking s to F_N
```

via reverse BFS from `accept` over the inverse of `δ_N`. The mask is constructed eagerly inside `PikeVm::new`. Cost is `O(|Q_N| + |δ_N|)` once.

(Pike VM cannot defer this the way the DFA path does, because the Pike VM's `match_dir` uses the mask on the first call. The wider project of "lazy reach-to-accept" applies only to the DFA hot path, which never uses the mask.)

## 5. `is_match` and `match_dir`

`is_match(w)`:

```
if ¬facts.accept(w): return false        // §05 prefilter
S_curr ← ε-closure({q₀})
for b in w:
    S_next ← ∅
    bump generation
    for q in S_curr:
        step(states[q], b, S_next, generation, at_segment_start)
    swap(S_curr, S_next)
    at_segment_start ← b ∈ Sep
    if S_curr is empty: return false
return S_curr ∩ accepts_at_eof ≠ ∅
```

`match_dir(d)` runs the byte loop without the suffix prefilter (a directory path may not yet carry the pattern's suffix), then queries the same combination as the DFA:

```
exact   ← S_curr ∩ accepts_at_eof ≠ ∅
S_sep   ← ε-closure({ q' : q ∈ S_curr, (q, sep, q') ∈ δ_N })
prefix  ← (S_sep ∩ accepts_at_eof ≠ ∅)
          ∨ (S_sep ∩ reach_to_accept ≠ ∅)
verdict ← combine(exact, prefix)         // see §06
```

## 6. Complexity

- **Time** per byte: `O(m)` amortized using bitset ε-closure; `O(m²)` worst case in the pure-`Vec<StateId>` formulation. The implementation uses the latter, as `m ≤ 4096` makes the constant factor tolerable.
- **Time** total: `O(n · m)`, with `n = |w|`.
- **Space**: `O(m)` for the two state sets and the visited table.
- **Construction**: `O(|Q_N| + |δ_N|)` for `compute_reach_to_accept` plus the `Thompson::compile` cost.

The asymptotic guarantees are independent of pattern shape: any pattern admissible to the parser admits an `O(n · m)` simulation, regardless of brace breadth or class complexity. The Pike VM is therefore a soundness floor — should any future change destabilize the DFA budget, the matcher remains predictable.

## 7. Worked example: simulating `a/*.ts` against `a/x.ts`

Reusing the NFA from §02 §6. The states are `q₀ … q₇, accept`. Pike VM starts from `S_curr = ε-closure({q₀}) = {q₀}` and processes each input byte.

`generation` starts at 0 and is bumped before each step. `visited` is sized `|Q_N| = 9`.

```
init:    current = {q₀}                         visited = [0,0,0,0,0,0,0,0,0]

byte 'a' (gen=1)
   step(q₀): Byte('a', next=q₁) — accepted, add_thread(q₁)
                add_thread enters q₁ (Byte) — visited[q₁]=1, push to next
   next = {q₁}                                  visited = [0,1,0,0,0,0,0,0,0]
   swap → current = {q₁}; at_segment_start = false

byte '/' (gen=2)
   step(q₁): Byte('/', next=q₂) — accepted, add_thread(q₂)
                q₂ is Jump(q₃); recurse add_thread(q₃)
                q₃ is Split(q₄, q₅); recurse add_thread(q₄), add_thread(q₅)
                q₄, q₅ are byte-consumers — visited[q₄]=visited[q₅]=2, pushed
                q₃ is ε-only — only its successors went to next
   next = {q₄, q₅}                              visited = [0,0,2,0,2,2,0,0,0]
   swap → current = {q₄, q₅}; at_segment_start = true (just consumed '/')

byte 'x' (gen=3)
   step(q₄): AnyNonSep, dot_protected.
       segment_start=true ∧ b='x'≠'.' — guard passes.
       'x' is non-sep → accept. next=q₃, recurse add_thread(q₃)
       q₃ Split → add_thread(q₄), add_thread(q₅) — visited[q₄]=visited[q₅]=3
   step(q₅): Byte('.', next=q₆). 'x'≠'.' — REJECT.
   next = {q₄, q₅}                              visited = [0,0,0,0,3,3,0,0,0]
   swap → current = {q₄, q₅}; at_segment_start = false

byte '.' (gen=4)
   step(q₄): AnyNonSep, dot_protected.
       segment_start=false → guard does not fire.
       '.' is non-sep → accept. next=q₃, ε-closure {q₄, q₅}
   step(q₅): Byte('.', next=q₆). '.' = '.' — accept.
       add_thread(q₆) — q₆ is Byte, push.
   next = {q₄, q₅, q₆}                          visited = [0,0,0,0,4,4,4,0,0]
   swap → current = {q₄, q₅, q₆}; at_segment_start = false

byte 't' (gen=5)
   step(q₄): non-sep 't' → next=q₃ → ε-closure {q₄, q₅}
   step(q₅): Byte('.', next=q₆). 't'≠'.' — REJECT.
   step(q₆): Byte('t', next=q₇). accepted. add_thread(q₇).
   next = {q₄, q₅, q₇}                          visited = [0,0,0,0,5,5,0,5,0]
   swap → current = {q₄, q₅, q₇}

byte 's' (gen=6)
   step(q₄): non-sep 's' → ε-closure {q₄, q₅}
   step(q₅): Byte('.', next=q₆). REJECT.
   step(q₇): Byte('s', next=accept). accepted. add_thread(accept).
       accept is Match — visited[accept]=6, push.
   next = {q₄, q₅, accept}                      visited = [0,0,0,0,6,6,0,0,6]
   swap → current = {q₄, q₅, accept}

end of input.
   current ∩ accepts_at_eof = {accept} ≠ ∅  ⇒  is_match = true
```

Three things to notice:

1. `current` never grows past 3 elements. The bound is `|Q_N| = 9`, so the simulator's per-byte work is `O(9)` regardless of input length.
2. Every `step` reads `visited[q]` and writes `visited[q] = generation` before pushing to `next`. The same `q` reachable through two ε-paths in one byte's step would hit `visited[q] == generation` on the second visit and be skipped.
3. `dot_protected` on `q₄` only fires once — at byte 3, where `at_segment_start` was true. From byte 4 onwards the flag is false and the guard is a no-op.

For comparison: the DFA (§03 §8) reaches the same conclusion in 6 indexed loads with no scratch buffers and no per-state dispatch. Pike VM pays its overhead in the inner `step` match arm but offers identical correctness with a hard `O(n · m)` worst case across all patterns.

## 8. Source map

`engine/pikevm.rs`:

- `pub struct PikeVm { thompson, facts, prefixes, reach_to_accept }`.
- `PikeVm::new(program, dot)`: compiles the Thompson NFA, computes `reach_to_accept`, extracts static prefixes (§06).
- `PikeVm::is_match(path)` / `PikeVm::match_dir(dir_path)`: §5.
- `Scratch` (private): the active-set scratch buffers.
- `add_thread`, `step`: ε-closure expansion and per-byte dispatch.

## 9. References

- Cox, R. (2009). Regular Expression Matching: the Virtual Machine Approach. https://swtch.com/~rsc/regexp/regexp2.html.
- Cox, R. (2007). Regular Expression Matching Can Be Simple And Fast. https://swtch.com/~rsc/regexp/regexp1.html.
- Thompson, K. (1968). Regular Expression Search Algorithm. _CACM_ 11(6):419–422.
