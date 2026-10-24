# 01. Glob as a Regular Language

## 1. The language

Let Σ = `{0, …, 255}` be the byte alphabet. GLOB_SPEC.md assigns to every well-formed pattern `P` a language `L(P) ⊆ Σ*`. This note shows that every such language is regular and draws the consequences for the matcher.

## 2. Regular languages

The class of regular languages over Σ is the smallest class that contains the empty language ∅, the language `{ε}` and every singleton `{a}` for `a ∈ Σ`, and is closed under

- finite union, `L₁ ∪ L₂`;
- concatenation, `L₁ · L₂ = { u · v : u ∈ L₁, v ∈ L₂ }`;
- Kleene closure, `L* = ⋃ₙ₌₀ Lⁿ`.

By the theorem of Kleene, and of Rabin and Scott, this class is exactly the class of languages that finite automata accept. It is also closed under complement and intersection.

## 3. Translation

Let `Sep ⊆ Σ` be the separator set of GLOB_SPEC §2.2 (`{0x2f}` on every platform; on Windows `{0x2f, 0x5c}`). Each construct of the dialect denotes a regular language:

| Construct        | Language                                  |
| ---------------- | ----------------------------------------- |
| literal byte `a` | `{a}`                                     |
| `?`              | `Σ \ Sep`                                 |
| `*`              | `(Σ \ Sep)*`                              |
| `[abc]`          | `{a, b, c}`                               |
| `[!abc]`         | `Σ \ (Sep ∪ {a, b, c})`                   |
| `**`             | `Σ*`                                      |
| `**/X` (leading) | `(Sep ∪ (Σ \ Sep)⁺ · Sep)* · L(X)`        |
| `a/**/b`         | `L(a) · Sep · (Σ* · Sep ∪ {ε}) · L(b)`    |
| `a/**`           | `L(a) · Sep · Σ*`                         |
| `{r₁, …, rₙ}`    | `⋃ᵢ L(rᵢ)`                                |
| `\X`             | `{X}`                                     |

Every row uses union, concatenation, Kleene closure and the complement of a finite set within the fixed alphabet Σ. The complement of a finite set is regular, as the class is closed under complement; since the alphabet is fixed, no automaton construction is needed to form it.

**Proposition 1.** For every well-formed pattern `P`, `L(P)` is regular.

*Proof.* By induction on the parse tree. An atom denotes a finite or cofinite set, which is regular. Concatenation, brace alternation and the globstar are the closure operations. The leading `!` inverts the matching predicate and is not part of the language (§5). ∎

Dot protection (GLOB_SPEC §12) intersects each wildcard's language at a segment start with `Σ* \ ({.} · Σ*)`, and case folding (§13) replaces each ASCII letter by the two-element set of its cases. Both preserve regularity.

## 4. Extglobs are excluded

The bash extglobs `@(p)`, `?(p)`, `*(p)` and `+(p)` denote `L(p)`, `L(p) ∪ {ε}`, `L(p)*` and `L(p) · L(p)*`, all available through the closure operations.

`!(p)` denotes the complement `Σ* \ L(p)`. The class is closed under complement, but the construction passes through a deterministic automaton: the NFA of `p` is determinized by the subset construction and its accepting set inverted. The bound of `2^{|p|}` states is attained by short patterns: `(a|b)*a(a|b)^{n−1}`, "the n-th byte from the end is `a`", has an NFA of `n + 1` states and a minimal DFA of `2ⁿ` states. One occurrence of `!(...)` can therefore exceed any fixed budget at compile time.

The matcher represents the positive structure of a pattern directly and simulates a nondeterministic automaton over it. Supporting `!(...)` would require determinization before the complement, with the exponential blow-up above. The dialect therefore keeps union, concatenation and Kleene closure as its only operations, and reserves the complement for the whole pattern (§5).

## 5. Whole-pattern negation

The leading `!` (GLOB_SPEC §11.1) is read at the level of the matching predicate:

```
match(!P, w) := ¬ match(P, w)        for every w ∈ Σ*
```

The matcher computes `match(P, w)` and inverts the result. No automaton for `Σ* \ L(P)` is built, and the language of `P` is unchanged. A run of `!` of even length cancels and one of odd length negates once; the parser counts the run (`parse/mod.rs::parse`) and `Ast::negated` holds its parity.

## 6. Three empty things

- **The empty pattern.** The string of length zero is rejected when parsed (`GlobError::Empty`). It denotes no language.
- **The empty string ε.** A member of `L(*)`, `L(**)`, `L({a,})` and every Kleene closure. Many patterns admit it.
- **The empty language ∅.** No well-formed pattern denotes it, with one exception: on Windows, where `\ ∈ Sep`, a literal or class `\` never matches a separator, so `\\` and `[\\]` match nothing (GLOB_SPEC §4.2, §8.2, §14.4). A pattern that requires one of them denotes ∅. Otherwise every compiled automaton has a reachable accepting state.

## 7. References

- Kleene, S. C. (1951). Representation of events in nerve nets and finite automata. In *Automata Studies*, Princeton University Press.
- Rabin, M. O. and Scott, D. (1959). Finite automata and their decision problems. *IBM Journal of Research and Development* 3(2), 114–125.
- Hopcroft, J. E., Motwani, R. and Ullman, J. D. (2006). *Introduction to Automata Theory, Languages, and Computation*, 3rd ed., chapters 2–4.
- Cox, R. (2007). Regular expression matching can be simple and fast. https://swtch.com/~rsc/regexp/regexp1.html.
