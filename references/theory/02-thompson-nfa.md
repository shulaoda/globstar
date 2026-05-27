# 02 — Thompson NFA

The matcher's NFA is built from the linear `Op` program produced by the lowering pass (`engine/ops.rs`) using Thompson's classical construction (Thompson, 1968), augmented with a single implementation-specific state, `DotGuard`, that captures glob's segment-start dot rule.

Implementation: `impl/crates/globstar/src/engine/thompson.rs`.

## 0. Pipeline

```
   pattern string                       "a/*.ts"
        │
        ▼
   ┌─────────┐
   │ parser  │  (parser.rs)
   └─────────┘
        │
        ▼   AST: Concat[Lit("a"), Sep, Star, Lit(".ts")]
   ┌─────────┐
   │ lower   │  (engine/ops.rs)
   └─────────┘
        │
        ▼   linear `Vec<Op>`:  [ Lit("a"), Sep, Star, Lit(".ts") ]
   ┌──────────┐
   │ Thompson │  (engine/thompson.rs)
   │ ::compile│
   └──────────┘
        │
        ▼   `Thompson { states: Vec<Trans>, initial, accept, accepts_at_eof }`
   ┌──────────────┐    ┌──────────────┐
   │ ThompsonDfa  │ or │   PikeVm     │   (consumers, see §03 / §04)
   │  ::build     │    │   ::new      │
   └──────────────┘    └──────────────┘
```

Thompson takes the linear `Vec<Op>` and emits one `Trans` per NFA state plus the wiring (ε-transitions) between fragments. The output `Thompson` value is shared by the two downstream engines: the eager DFA when subset construction fits inside `MAX_DFA_STATES`, the Pike VM when it does not.

## 1. Definition

A **Thompson NFA fragment** for an `Op` program is a tuple `F = (Q, Σ, δ, q₀, f)` where

- `Q` is a finite set of states;
- `Σ ⊆ {0, 1, ..., 255} ∪ {ε}` is the input alphabet (we admit ε-transitions);
- `δ ⊆ Q × Σ × Q` is the transition relation;
- `q₀ ∈ Q` is the initial state;
- `f ∈ Q` is the unique accepting state.

The Thompson construction is recursive on the structure of the input, and at every step it produces a fragment with exactly one initial state and exactly one accepting state.

## 2. Constructions

The presentation below states the fragment for each `Op`. All constructions are standard except `OptSegmentsSlash`, `SlashAnything`, `GlobstarAny`, `LeadingSeps`, and `DotGuard`, which encode glob-specific semantics.

### 2.1 Atomic constructions

For `Op::Lit(b₁ b₂ … bₖ)`:

```
q₀ ─b₁─→ q₁ ─b₂─→ q₂ ─ … ─→ qₖ = f
```

A chain of `Trans::Byte { b, next }` states.

For `Op::AnyChar`:

```
q₀ ─Σ\Sep─→ f
```

A `Trans::AnyNonSep { next: f, dot_protected }` state.

For `Op::Class(C)`:

```
q₀ ─C─→ f
```

A `Trans::Class { class: C, next: f, dot_protected }` state.

For `Op::Sep`:

```
q₀ ─Sep─→ f
```

A `Trans::Sep { next: f }` state. The class `Sep` is the platform separator set defined in §1.

The `dot_protected: bool` flag is set on `AnyNonSep`, `AnyByte`, and `Class` whenever the construction places the state at a segment- start position under `dot = false`. The runtime semantics of the flag are given in §4.

### 2.2 Concatenation

For `Op` sequences `s₁ s₂`, build the fragments separately and introduce an ε-transition from the accept of `F(s₁)` to the initial of `F(s₂)`:

```
q₀(F(s₁)) ─F(s₁)─→ f(F(s₁)) ─ε─→ q₀(F(s₂)) ─F(s₂)─→ f(F(s₂))
```

ε-transitions are realized by `Trans::Jump { next }`.

### 2.3 Alternation

For brace `{r₁, …, rₙ}`, lowered to `Op::Alternation`:

```
              ┌─ε─→ q₀(F(r₁)) ─F(r₁)─→ f(F(r₁)) ─ε─┐
              │                                      │
   q₀ ──ε─→ split                                  rejoin = f
              │                                      │
              └─ε─→ q₀(F(rₙ)) ─F(rₙ)─→ f(F(rₙ)) ─ε─┘
```

`split` is a chain of `Trans::Split { a, b }` states each splitting into one branch and the next split. Rejoin is one `Trans::Jump` per branch into a shared `f`.

### 2.4 Kleene closure (single-segment `*`)

`Op::Star` (under separator avoidance) compiles to:

```
              ┌─ε─→ AnyNonSep ─ε─┐
              │                    │
   q₀ ──ε─→ split                  ↓
              │                    rejoin = f
              └────────ε──────────┘
                                   ↑
              ┌─────ε──────────────┘
              │
              └─ε─→ AnyNonSep ─ε─→ split        (loop)
```

A self-loop on `AnyNonSep` plus an ε-bypass to allow zero repetitions. The dot protection flag, when set on the `AnyNonSep`, is enforced at every loop entry by the `DotGuard` (§2.7).

### 2.5 `OptSegmentsSlash`

`Op::OptSegmentsSlash` denotes `((Σ \ Sep)⁺ · Sep)*`, i.e. zero or more "segment + separator" repetitions. The fragment uses two nested loops:

```
        AnyNonSep ─ε─→ Sep ─ε─→ f
            ▲          ▲         ↑
            └Split─────┘  ←Split─┘
```

The inner self-loop consumes a non-empty segment body; the outer loop consumes the segment terminator and resumes. Both loops admit the empty case via ε-bypasses; the construction therefore matches zero-or-more repetitions, and zero is `ε`.

### 2.6 `SlashAnything` and `GlobstarAny`

`Op::SlashAnything` denotes `Sep · Σ*`:

```
   q₀ ─Sep─→ AnyByte (self-loop) ─ε─→ f
```

`Op::GlobstarAny` denotes `Σ*`:

```
   q₀ ─AnyByte (self-loop)─→ f
```

Dot protection is set on each `AnyByte` self-entry under `dot = false`.

### 2.7 `DotGuard` (the single non-classical addition)

The segment-start dot rule of glob (`*`, `?`, and negated character classes do not consume `.` at a segment start under `dot = false`) is byte-conditional: the decision depends on the upcoming byte. We therefore cannot collapse it into ε-closure.

`Trans::DotGuard { next }` is an ε-state with the following operational semantics:

- in ε-closure expansion, `DotGuard` is admitted to the set but its outgoing edge is not taken;
- in the per-byte step, the state evaluates `at_segment_start ∧ upcoming_byte = b'.'`:
  - if the conjunction holds, the thread originating in the `DotGuard` dies (no successor);
  - otherwise, the thread proceeds via ε to `next` and is then advanced by the byte;
- at end-of-input, the guard is satisfied vacuously and the thread is treated as ε-reaching `next`.

The construction places `DotGuard` immediately before each `AnyNonSep` / `AnyByte` / negated `Class` whose position is a segment start.

## 3. The compiled NFA

`Thompson::compile(program, dot)` returns a structure `(states, initial, accept, accepts_at_eof)` where:

- `states: Vec<Trans>` — the union of fragments produced by each `Op`, with ε-transitions wiring fragments per §2;
- `initial: StateId` — `q₀` of the outermost fragment;
- `accept: StateId` — the unique `Trans::Match` state;
- `accepts_at_eof: BitMask` — the set of states from which `accept` is reachable using ε-closure alone (`DotGuard` included; see §2.7).

`accepts_at_eof` is the subset of `Q` whose membership ends matching: `is_match(w)` iff after consuming `w` the active state set intersects `accepts_at_eof` non-trivially.

## 4. Runtime semantics of the active set

Define ε-closure as the standard reflexive-transitive closure under `Trans::Split`, `Trans::Jump`, and `DotGuard`-as-ε:

```
ε-closure(S) = { q' : ∃ q ∈ S, q →ε* q' }
```

The active set evolves per byte `b`:

```
S₀ = ε-closure({q₀})
Sᵢ₊₁ = ε-closure({ q' :  q ∈ Sᵢ ∧ (q, b, q') ∈ δ
                          ∧ DotGuard predicate satisfied })
```

`is_match(w)` ≡ `S_{|w|} ∩ accepts_at_eof ≠ ∅`.

## 5. Size bounds

**Proposition 2.** `|Q| ≤ c · |program|` for a constant `c` depending only on the construction rules in §2.

For our specific rules, `c ≤ 5`: each atomic op contributes at most 1 state, each concatenation contributes 1 ε-state, each alternation contributes `n + 1` ε-states for `n` branches, and the segment-aware ops contribute at most 4 states each.

The bound matters because the `MAX_DFA_STATES = 4096` cap in §03 operates on subsets of `Q`. The size of `Q` directly bounds the fast-dedup feasibility (§03 §4).

## 6. Worked example: `a/*.ts` matching `a/x.ts`

### 6.1 Lowering

```
pattern  : "a/*.ts"
↓ parser
AST      : Concat[ Lit("a"), Sep, Star, Lit(".ts") ]
↓ lower (engine/ops.rs)
Vec<Op>  : [ Lit("a"),  Sep,  Star,  Lit(".ts") ]
            └ op[0] ─┘ └op[1]┘ └op[2]┘ └ op[3] ─┘
```

### 6.2 Thompson NFA

`Thompson::compile` produces 7 states plus the accept:

```
   q₀ ─'a'─→ q₁ ─'/'─→ q₂ ─ε─→ q₃        Star body
                                ↑   │
                                │   ▼
                                │  q₄ (AnyNonSep, dot_protected=true at q₂)
                                │   │
                                │   ▼
                                │  q₃ (back-edge: Split{a:q₄,b:q₅})
                                │
                                ▼
                                q₅ ─'.'─→ q₆ ─'t'─→ q₇ ─'s'─→ q₈ = accept
```

Concretely the `states: Vec<Trans>` array is:

```
q₀: Byte { b='a',  next=q₁ }
q₁: Byte { b='/',  next=q₂ }
q₂: Jump { next=q₃ }                        // ε into the *-loop entry
q₃: Split { a=q₄, b=q₅ }                    // ε bypass: enter loop OR exit
q₄: AnyNonSep { next=q₃, dot_protected=true } // *-loop body, back to q₃
q₅: Byte { b='.',  next=q₆ }
q₆: Byte { b='t',  next=q₇ }
q₇: Byte { b='s',  next=accept }
accept: Match
```

`q₂ → q₃` and the bypass `q₃ → q₅` give the "zero or more iterations" of the `*`. The `dot_protected=true` flag on `q₄` honors the segment-start dot rule (see §2.7).

`accepts_at_eof = { accept }` only — none of `q₀ … q₇` ε-reach `accept` without consuming bytes.

### 6.3 Active-set evolution on input `"a/x.ts"`

`S₀ = ε-closure({q₀}) = {q₀}` (q₀ is a byte-consuming state, ε-closure stops there).

| Step | Byte    | Active set after step                                                                                                                                                                  | Notes                                                                                                                                           |
| ---- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| 0    | (start) | `{q₀}`                                                                                                                                                                                 | initial                                                                                                                                         |
| 1    | `a`     | `{q₁}`                                                                                                                                                                                 | `q₀ ─a─→ q₁`                                                                                                                                    |
| 2    | `/`     | `{q₂}` then ε-close to `{q₃, q₄, q₅}`                                                                                                                                                  | `q₂ ε→ q₃`, `q₃` splits into `q₄` (loop) and `q₅` (exit). At `q₂` the segment-start flag flips to true; `q₄`'s dot-protection is now in effect. |
| 3    | `x`     | `{q₃, q₄, q₅}` consume `x`. `q₃` and `q₅` are ε-states (no byte transition). `q₄` accepts non-sep `x` (not `.`, so dot guard passes), produces `{q₃}` then ε-closes to `{q₃, q₄, q₅}`. | After this byte segment-start flips to false.                                                                                                   |
| 4    | `.`     | from `{q₃, q₄, q₅}`: `q₄` (now NOT segment-start) accepts `.` → back to `{q₃, q₄, q₅}`; `q₅` accepts `.` → `{q₆}`. Union: `{q₃, q₄, q₅, q₆}`.                                          |                                                                                                                                                 |
| 5    | `t`     | `q₃, q₄, q₅` reject `.` was step 4; on `t`, `q₄` accepts → `{q₃, q₄, q₅}`; `q₆` accepts → `{q₇}`. Union: `{q₃, q₄, q₅, q₇}`.                                                           |                                                                                                                                                 |
| 6    | `s`     | `q₄` accepts → `{q₃, q₄, q₅}`; `q₇` accepts → `{accept}`. Union: `{q₃, q₄, q₅, accept}`.                                                                                               |                                                                                                                                                 |

End of input. `S₆ ∩ accepts_at_eof = {accept} ≠ ∅`, so `is_match("a/x.ts") = true`.

The example exposes three properties worth noting:

1. **The `*`-loop is alive throughout**. `q₃, q₄, q₅` persist in the active set across many bytes — the simulation maintains both "still inside the loop" and "exited the loop" possibilities until input determines which one reaches accept.
2. **Dot protection only fires at byte 3**, the first byte after `/`. By byte 4 the segment-start flag is false, and the `.` is consumed normally inside the segment.
3. **The accept state is reachable only via `q₅ ─'.'─→ q₆ ─'t'─→ q₇ ─'s'─→ accept`**. The exit branch from `q₃` is the only path; the loop body alone never reaches accept.

## 7. Source map

`engine/thompson.rs`:

- `Trans` enum (§2): `Byte`, `Class`, `AnyNonSep`, `AnyByte`, `Sep`, `Split`, `Jump`, `DotGuard`, `Match`.
- `Thompson` struct: `states`, `initial`, `accept`, `accepts_at_eof`.
- `Thompson::compile(program, dot)`: entry point.
- `compute_reach_to_accept(states, accept)`: backward reachability used for `match_dir` (§06).

## 8. References

- Thompson, K. (1968). Regular Expression Search Algorithm. _CACM_ 11(6):419–422.
- Cox, R. (2007). Regular Expression Matching Can Be Simple And Fast. https://swtch.com/~rsc/regexp/regexp1.html.
- Cox, R. (2009). Regular Expression Matching: the Virtual Machine Approach. https://swtch.com/~rsc/regexp/regexp2.html. (Pike VM, see §04.)
