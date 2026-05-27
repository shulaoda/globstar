# 01 — Glob as a Regular Language

## 1. The dialect under study

Let Σ = `{0, 1, ..., 255}` be the byte alphabet. The glob dialect defined in `references/spec/GLOB_SPEC.md` denotes a language `L(P) ⊆ Σ*` for every well-formed pattern `P`. This note shows that the dialect is regular and identifies the consequences for the matcher implementation.

## 2. The closure operations

A class of languages is _regular_ if it contains the singletons `{a}` for every `a ∈ Σ`, the empty language `∅`, and the language `{ε}`, and is closed under

- finite union (`L₁ ∪ L₂`),
- concatenation (`L₁ · L₂ := { uv : u ∈ L₁, v ∈ L₂ }`), and
- Kleene closure (`L* := ⋃_{n ≥ 0} Lⁿ`).

By the Kleene–Rabin–Scott equivalence, the class so defined coincides with the languages accepted by finite automata.

## 3. Translation table

Each glob construct denotes a regular operation on Σ. Letting `Sep ⊆ Σ` denote the platform separator set (`{0x2f}` always; on Windows additionally `{0x5c}`; see `references/spec/GLOB_SPEC.md` §12.3), we have:

| Construct        | Language                                   |
| ---------------- | ------------------------------------------ |
| literal byte `a` | `{a}`                                      |
| `?`              | `Σ \ Sep`                                  |
| `*`              | `(Σ \ Sep)*`                               |
| `[abc]`          | `{a, b, c}`                                |
| `[!abc]`         | `Σ \ (Sep ∪ {a, b, c})`                    |
| `**`             | `Σ*`                                       |
| `**/X` (lead)    | `Sep* · ((Σ \ Sep)⁺ · Sep)* · L(X)`        |
| `a/**/b` (mid)   | `L(a) · Sep⁺ · ((Σ \ Sep)⁺ · Sep)* · L(b)` |
| `a/**` (trail)   | `L(a) · Sep · Σ*`                          |
| `{r₁, ..., rₙ}`  | `⋃ᵢ L(rᵢ)`                                 |
| `\X`             | `{X}`                                      |

Each row uses only union, concatenation, complement-of-finite-set, and Kleene closure. The complement of a finite set (`Σ \ Sep`, `Σ \ (Sep ∪ {a,b,c})`) is regular because finite sets are regular and the regular class is closed under complement; here the complement is taken against a fixed alphabet, so no DFA construction is required.

**Proposition 1.** For every well-formed pattern `P`, `L(P)` is regular.

_Proof sketch._ By induction on the parse tree. Atomic nodes denote finite or co-finite sets, hence regular. Concatenation, brace alternation, and the Kleene-style globstar are exactly the closure operations. The leading `!` is a top-level boolean inversion of the matching predicate; it is not part of the language definition (see §5). ∎

## 4. The exclusion of bash extglobs

Bash extglobs

```
@(p)   ?(p)   *(p)   +(p)   !(p)
```

are out of scope. The first four denote `L(p)`, `L(p) ∪ {ε}`, `L(p)*`, and `L(p) · L(p)*` respectively, all of which are regular operations already in our toolkit.

`!(p)` is the language complement `Σ* \ L(p)`. Regularity is preserved: the regular class is closed under complement. The construction proceeds via subset construction followed by toggling the accepting set:

```
NFA(p) ─subset─→ DFA(p) ─toggle accept set─→ DFA(¬p)
```

The cost is a tight upper bound of `2^{|p|}` DFA states. This bound is reached for natural patterns (e.g. `(a|b)^n`); a single occurrence of `!(...)` therefore suffices to push the compile-time state count past any reasonable matcher budget.

The implementation enforces a state cap `MAX_DFA_STATES = 4096` during subset construction (§03). Permitting `!(...)` would make the cap a per-pattern lottery: a one-character edit to the user's input could move it from "compiles to a DFA" to "falls back to NFA simulation" without the user being able to reason about the boundary. Excluding `!(...)` keeps the cap a function linear in `|P|` rather than exponential, preserving compile-time predictability.

The dialect therefore restricts to union, concatenation, and Kleene closure as primitives; the complement is reserved for the top-level predicate inversion described next.

## 5. Whole-pattern negation by predicate inversion

The leading `!` (`references/spec/GLOB_SPEC.md` §9.3) is interpreted at the matching predicate level rather than at the language level:

```
match(!P, w) := ¬match(P, w)         for all w ∈ Σ*
```

The matcher computes `match(P, w)` and inverts the boolean. No automaton over `Σ* \ L(P)` is constructed; the language definition of `P` is unchanged.

Even-numbered runs of leading `!` cancel; odd-numbered runs negate once. This is the parity rule formalized in `references/spec/GLOB_SPEC.md` §9.5 and implemented in the parser (`parser.rs::parse_negation`).

## 6. Empty pattern; empty string; the empty language

Three distinct notions:

- **Empty pattern.** The input string of length zero is rejected at parse time (`GlobError::Empty`). It denotes nothing in the language.
- **Empty string ε.** A member of `L(*)`, of `L(**)`, of `L({a,})`, and of every Kleene-starred language. Several patterns admit it.
- **Empty language `∅`.** Not denotable by any well-formed pattern. The matcher never compiles to a `Match`-unreachable NFA from a legal source input.

## 7. References

- Kleene, S. C. (1951). Representation of events in nerve nets and finite automata. In _Automata Studies_, Princeton.
- Rabin, M. O. and Scott, D. (1959). Finite automata and their decision problems. _IBM J. Res. Dev._ 3(2):114–125.
- Hopcroft, J. E., Motwani, R., and Ullman, J. D. _Introduction to Automata Theory, Languages, and Computation_, ch. 2–4.
- Cox, R. (2007). Regular Expression Matching Can Be Simple And Fast. https://swtch.com/~rsc/regexp/regexp1.html.
