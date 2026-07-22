# GLOB_SPEC — Authoritative Syntax and Semantics

> This document is the **source of truth** for the glob dialect implemented by the `globstar` Rust crate (`impl/crates/globstar`) and the `@globstar/globstar` JS package (`impl/packages/globstar`). Any discrepancy between code, tests, documentation, and this spec is resolved by changing the code. Changes to the spec itself follow the discussion + revision procedure recorded in `decisions/`. **Version:** v0.2.0 (draft) **Status:** active — not yet released, still mutable. **Baseline:** `picomatch` + `fast-glob` mainline behavior, with the rigor of `doublestar`. We do NOT aim for bit-exact compatibility with any single tool. **Normative keywords:** "MUST", "SHOULD", "MAY" follow RFC 2119.

## Table of Contents

1. [Scope and Compatibility](#1-scope-and-compatibility)
2. [Character Set and Input Conventions](#2-character-set-and-input-conventions)
3. [BNF Grammar](#3-bnf-grammar)
4. [Semantics of Basic Constructs](#4-semantics-of-basic-constructs)
5. [Extglobs (out of scope)](#5-extglobs-out-of-scope)
6. [Character Class Semantics](#6-character-class-semantics)
7. [Brace Expansion Semantics](#7-brace-expansion-semantics)
8. [Path Separator and `**` Semantics](#8-path-separator-and--semantics)
9. [Escapes and the Leading `!`](#9-escapes-and-the-leading-)
10. [Compile-time Errors](#10-compile-time-errors)
11. [Options](#11-options)
12. [Match Semantics](#12-match-semantics)
13. [Directory Matching `match_dir`](#13-directory-matching-match_dir)
14. [Multi-pattern Matchers](#14-multi-pattern-matchers)
15. [Edge Case Catalog](#15-edge-case-catalog)
16. [Differences from Other Tools](#16-differences-from-other-tools)
17. [Appendix A: Version History](#17-appendix-a-version-history)
18. [Appendix B: Glossary](#18-appendix-b-glossary)

---

## 1. Scope and Compatibility

### 1.1 In scope

This specification defines the glob pattern language and matcher semantics, covering:

- **Standard glob constructs** — `*`, `**`, `?`, character classes `[...]`, brace expansion `{...}`, escape `\`, leading-`!` negation.
- **Match semantics** — what `is_match(pattern, path)` returns for every pair of inputs.
- **Directory matching** — a first-class `match_dir` API that returns `Pruned` / `Descend` / `Match` / `DescendAndMatch`. Filesystem walkers consume this; the matcher just defines what each result means.
- **Multi-pattern matchers** — boolean OR over a set of patterns (the `globstar(patterns)` factory in JS, `Glob::union` / `GlobSet` equivalents in Rust).

Filesystem traversal — pattern preprocessing for the walker, cwd validation, the auto-split of `!`-prefixed patterns into the ignore set, traversal order, and walker-level error handling — is the subject of a separate spec, [WALKER_SPEC.md](./WALKER_SPEC.md).

### 1.2 Out of scope

The following constructs are explicitly NOT supported:

- **Bash extglobs**: `@()`, `?()`, `*()`, `+()`, `!(...)` are matched as literal byte sequences (the inner `?` and `*` retain their wildcard meanings; the parentheses themselves are literal).
- **POSIX character classes**: `[[:alpha:]]`, collating elements `[[.a.]]`, equivalence classes `[[=a=]]`.
- **Bash brace ranges**: `{1..10}`, `{a..z}`.
- **Regex features**: backreferences `\1`, capturing groups `(...)`, lookahead/lookbehind.
- **Gitignore "re-include" semantics**: the "the next rule cancels the previous" behavior must be implemented at the walker / GlobSet layer.
- **Full Unicode case folding**: only an ASCII case-insensitive flag is provided (§11.2).
- **Shell-specific constructs**: `~` home expansion, `$VAR` variable expansion.

### 1.3 Compatibility statement

This spec does NOT target bit-exact behavioral parity with any single tool. The closest reference is `picomatch`, with deliberate divergences enumerated in §16.

---

## 2. Character Set and Input Conventions

### 2.1 Character set

Patterns are byte sequences. UTF-8 well-formedness is NOT required.

- Implementations MUST accept arbitrary `&[u8]` (Rust) / `Uint8Array` (JS) / byte-encodable string input.
- Implementations MAY provide a `&str` / JS-string convenience entry that skips one UTF-8 round-trip on the hot path.
- Bytes within the ASCII range MAY carry meta-character meaning. Bytes outside the ASCII range are ALWAYS treated as literal bytes.
- Unicode codepoint boundaries do NOT influence matching — comparisons are byte-for-byte.

### 2.2 Path separator in patterns

**Inside a pattern, `/` is the only path separator, regardless of the host platform.**

- ✅ `src/main.rs`
- ✅ `src/components/*.tsx`
- ❌ `src\main.rs` — not even on Windows.
- ❌ `src\\main.rs` — this parses as "literal `src` + escaped `\` + `main.rs`", not a path with backslash separators.

This rule eliminates cross-platform pattern ambiguity. If a tool needs to accept native-shell paths as patterns, it MUST normalize to `/` before calling the matcher.

### 2.3 Path separator in matched paths

The path being matched MAY use either `/` (always accepted) or the platform's native separator. The matcher recognizes an **implementation-defined separator byte set `Seps`**:

- `/` ∈ `Seps` (specification mandate);
- additional bytes (typically `\`) are at the implementation's discretion, conventionally following platform norms;
- the reference implementation defines `Seps := { b : std::path::is_separator(b as char) }`, i.e. `\` belongs to `Seps` on Windows and nowhere else.

Cross-platform consequences are detailed in §12.3.

### 2.4 Escape character

The byte `\` is ALWAYS the escape character inside a pattern. The byte that follows it is interpreted as:

- meta-character (`* ? [ ] { } ! ( ) \ |`) → produces that literal byte;
- any other byte → produces that literal byte (lenient policy: `\a` ≡ `a`);
- end of pattern → **parse error** (`TrailingBackslash`).

---

## 3. BNF Grammar

```bnf
Pattern      ::= [Negation] Sequence

Negation     ::= "!"+         ; leading `!`-run, parity decides final negation

Sequence     ::= Atom*

Atom         ::= Literal
             |  Separator
             |  AnyChar
             |  Star
             |  Globstar
             |  CharClass
             |  Brace
             |  Escaped

Literal      ::= a single non-meta byte
Separator    ::= "/"
AnyChar      ::= "?"
Star         ::= "*"          ; only when not part of a Globstar
Globstar     ::= "**"         ; MUST own a path segment (§8)

CharClass    ::= "[" ("^"|"!")? CharClassItem+ "]"
CharClassItem::= Literal
             |  Range
             |  Escaped
Range        ::= Literal "-" Literal

Brace        ::= "{" BraceList "}"
BraceList    ::= Sequence ("," Sequence)*
              ; empty branches `{a,}` are allowed and produce an empty branch

Escaped      ::= "\" AnyByte    ; \X yields literal X
```

### Full meta-character list

```
*  ?  [  ]  {  }  \  ,
```

Notes:

- `|` is a branch separator only inside a brace; in any other position it is a literal byte.
- `,` is a branch separator only inside a brace.
- `!` is a path-level negation marker only at the **very start** of the pattern (§9). Anywhere else it is a literal byte.
- `@`, `+`, `(`, `)` are ALWAYS literal bytes.

---

## 4. Semantics of Basic Constructs

### 4.1 `Literal`

Matches the given byte exactly once.

### 4.2 `?` (single character)

Matches exactly one byte that is NOT a path separator (`/`, plus any implementation-defined member of `Seps`).

- Does NOT match the empty string.
- Does NOT match `/`.
- Interaction with the `dot` option: when `dot = false` (default at the walker layer), `?` at a segment-start position MUST NOT match `.`. See §12.4.

### 4.3 `*` (single-segment wildcard)

Matches zero or more non-separator bytes.

- MAY match the empty string (`foo*bar` matches `foobar`).
- MUST NOT cross `/` (`src/*` does not match `src/a/b`).
- Interaction with `dot`: when `dot = false`, `*` at the first character of a segment MUST NOT consume `.` (so `*.txt` does not match `.txt`).

### 4.4 `**` (recursive wildcard)

See §8 — too rich to compress into this section.

### 4.5 Empty pattern `""`

**Parse error** (`Empty`). The empty pattern has no obvious semantics, so the parser rejects it explicitly.

---

## 5. Extglobs (out of scope)

Bash extglobs `@()`, `?()`, `*()`, `+()`, `!(...)` are NOT supported. The parser treats them as literal byte sequences per the §3 grammar (the inner `?` and `*` retain their wildcard meanings; `(`, `)`, `@`, `+`, `|` are literal bytes).

For alternation use a brace `{a,b}`. For top-level negation use the leading-`!` form (§9).

---

## 6. Character Class Semantics

### 6.1 Basic forms

```
[abc]       set:    matches a, b, or c
[a-z]       range:  matches any byte in [a..=z]
[abc0-9]    mixed   set + range
[!abc]      negated set: matches any byte that is none of a, b, c
[^abc]      negated set: equivalent to [!abc]
```

### 6.2 Character classes are segment-local

A `[...]` MUST be closed inside the segment that contains it. `/` is the sole spec-level segment delimiter (§12.3); encountering `/` inside a class body is treated as the segment ending before the class closer was found, which from the class parser's point of view is "EOF without `]`": `UnterminatedClass`.

`\` is NOT a spec-level separator — it is the pattern-level escape character. A literal `\` byte MAY appear inside a class via the doubled-`\` form (`\\`). The class membership rules treat `\` like any other byte member.

Concrete cases:

- explicit `/` in the class: `[/]`, `[a/b]`, `[/abc]`, `[^/]` — all `UnterminatedClass`;
- escaped `/`: `\/` resolves to a literal `/` — also `UnterminatedClass`;
- literal `\` (written as `\\` in the pattern): legal — class may contain the `\` byte.

`CharClass::matches(b)` runtime rules (where `Seps` is the implementation-defined separator set, §12.3):

- **Classes never match a `Seps` member, regardless of polarity.** This preserves the segment-local invariant. `/` ∈ `Seps` is a spec mandate; in the reference implementation `\` is in `Seps` on Windows only.
- For negated classes this prevents `[^abc]` from sneaking `/` (or Windows-`\`) into the match via the trivial "anything-except-set" reading. `[^abc]` rejects `\` on Windows but allows `\` on Unix (where `\` is an ordinary byte, not a separator).
- For positive classes a user who writes `[\\]` to match a literal `\` byte gets a **platform-dependent silent miss** on systems where `\` ∈ `Seps` (Windows), because the segment-local invariant rules out matching a separator from within a class. On Unix `[\\]` matches `\` normally. This is the natural extension of the `[/]` parser rejection.

**Why parse error for `[/]` rather than silent ignore.** Silent ignore would compile `foo[/]bar` successfully but match nothing — the worst possible UX. A parse error surfaces the incompatibility immediately.

### 6.3 Special characters inside a class

Per-byte role assignment inside a class body:

| Byte                                   | Role                | Trigger                                                           |
| -------------------------------------- | ------------------- | ----------------------------------------------------------------- |
| `]`                                    | **closer**          | not in first position (in first position, see §6.5)               |
| `-`                                    | **range separator** | between two non-`]` bytes; literal when adjacent to `[` or `]`    |
| `!` / `^`                              | **negation marker** | immediately after `[` (overridable by escape)                     |
| `\`                                    | **escape prefix**   | always — consumes the next byte as literal                        |
| `/`                                    | **illegal**         | any position → `UnterminatedClass` (§6.2)                         |
| `*`, `?`, `{`, `}`, `(`, `)`, `@`, `+` | literal             | always (outside-class meta-characters degrade to literals inside) |
| any other byte                         | literal             | always                                                            |

Escape rules (consistent with the lenient §9.1 strategy):

- `\X` → literal `X`, regardless of whether `X` is a meta-character;
- `[\a]` ≡ `[a]`; `[\?]` matches `?`; `[\-]` matches `-` (literal, not range);
- `[\!]` ≡ `[!]` literal (overrides the "first-`!` is negation" rule);
- `[\]]` matches `]` (`\` consumes the first `]`; the next `]` is the closer);
- `[\]` is **illegal** — `\` consumes the lone `]`, leaving items but no closer → `UnterminatedClass`;
- `[\\]` (two backslashes in the source pattern) is the class containing the literal `\` byte. Runtime behavior is **platform-dependent** per §6.2 / §12.3.

Corner-case summary:

| Pattern             | Result                  | Rationale                                                              |
| ------------------- | ----------------------- | ---------------------------------------------------------------------- |
| `[?*!(]`            | class `{?, *, !, (}`    | meta degraded to literal in class body                                 |
| `[a-z]`             | range `a..=z`           | `-` between non-bracket bytes                                          |
| `[a\-z]`            | class `{a, -, z}`       | `\-` blocks the range, `-` is literal                                  |
| `[-abc]` / `[abc-]` | class with literal `-`  | `-` adjacent to a bracket is literal                                   |
| `[!abc]`            | negated `{a, b, c}`     | `!` in first position                                                  |
| `[\!abc]`           | positive `{!, a, b, c}` | escape overrides negation                                              |
| `[]]`               | class `{]}`             | POSIX first-`]` rule (§6.5)                                            |
| `[!]]`              | negated `{]}`           | first-`]` + negation                                                   |
| `[\]]`              | class `{]}`             | escaped form                                                           |
| `[\]`               | `UnterminatedClass`     | `\` consumes `]`, no closer remains                                    |
| `[]`                | `UnterminatedClass`     | first-`]` is literal, then no closer                                   |
| `[/]` / `[\/]`      | `UnterminatedClass`     | separator illegal in class (§6.2)                                      |
| `[\\]`              | class `{\}`             | platform-dependent runtime — Unix matches `\`, Windows silently misses |

### 6.4 Negated classes and the `dot` option

`[^x]` may or may not match `.` depending on `dot`:

- `dot = true`: `[^x]` may consume `.` at any segment-start position;
- `dot = false`: it MUST NOT.

This keeps "dot-protection by default" consistent across all wildcard constructs (see §12.4 for the precise positional definition).

### 6.5 Empty classes and the POSIX first-`]` rule

- `[]` → `UnterminatedClass` — the first `]` is treated as a literal member (POSIX rule); no closer follows.
- `[` (unclosed) → `UnterminatedClass`.
- `[!]` / `[^]` — same outcome: the first `]` after the negation is a literal member, leaving items but no closer → `UnterminatedClass`.
- `[a-]` (no upper bound) — the trailing `-` becomes a literal member (lenient).

The first-`]` rule means `[]]` is legal and matches `]`; `[!]]` matches any byte except `]`; `[]-]` matches `]` or `-`.

### 6.6 POSIX character classes

NOT supported. `[[:alpha:]]` is parsed as `[` opening a class, then `[`, `:`, `a`, ... ; the result almost always errors out.

---

## 7. Brace Expansion Semantics

### 7.0 Normative definition: the expansion equation

A brace is **pure union**. For any pattern fragments `pre`, `post` and
branches `A`, `B`, …:

```
L( pre{A,B,…}post ) = L( preApost ) ∪ L( preBpost ) ∪ …
```

Each expansion is interpreted as a standalone pattern under every rule
of this specification — in particular, **`**` segment ownership (§8.1)
is judged on the expanded form**: a brace neither grants `**` powers
it lacks outside (`a{**,x}b` ≡ `a{*,x}b` ∪ `axb` — the `**` degrades
because its expanded neighbors are `a`/`b`) nor takes away powers it
has (`{**,x}/b` ≡ `**/b` ∪ `x/b`, which matches `b`).

Two exceptions, without which the equation would over-apply:

1. **Leading `!` is processed before expansion** (§9.3). `{!a,b}`
   expands to the body `!a` whose `!` is a literal byte — expansion
   never re-triggers pattern-level negation.
2. **An empty expansion denotes ε, not a parse error.** `{a,}`'s empty
   branch contributes the empty string to the union; the `Empty`
   error (§10) applies only to the user-supplied pattern itself.

Implementation note: the equation defines the **semantics**; §7.2's
no-pre-expansion rule still governs the implementation, which must
merely be equivalent. One corner is resolved textually rather than by
full expansion: a single separator flanked by TWO globstar-edged
braces (`{a,**}/{**,b}`) is owned by the left brace's branch tails;
implementations MAY deviate from the pure equation on that shape as
long as both reference runtimes agree.

### 7.1 Basic forms

```
{a,b,c}     matches a, b, or c
{foo,bar}   literal-branch alternatives
{a*b,c?}    branches may contain wildcards
{a,{b,c}}   nested braces
{a,b}{c,d}  Cartesian product (parsed independently, concatenated at match time)
```

### 7.2 No pre-expansion

Braces are embedded into the matcher (NFA / backtracker / brace-aware DFA); they are NOT pre-expanded into N independent patterns at parse time. This keeps memory and match-time costs immune to the combinatorial blow-up of `{a,b}{c,d}{e,f}...`.

### 7.3 Empty branches

`{a,}` is **legal** and equivalent to `(a|ε)` — matches `a` or the empty string.

### 7.4 Single-branch braces

`{a}` (no comma) is NOT a brace. It is parsed as the literal four-byte sequence `{a}`. This is the consensus of Bash, picomatch, and fast-glob.

### 7.5 Unclosed braces

`{a,b` → **parse error** (`UnterminatedBrace`).

### 7.6 Nesting depth limit

Braces MAY be nested up to 32 levels deep. Exceeding this limit is a parse error (`BraceNestingTooDeep`, see §10).

### 7.7 Cartesian-product cap

Branch counts and Cartesian-product breadth are NOT bounded by the parser because we do not pre-expand. The matcher handles them through standard NFA / backtracking, with cost asymptotically equivalent to the naive brace.

### 7.8 Braces and `**`

A brace branch MAY contain `**`:

```
{src/**/*.ts,lib/**/*.js}
```

The two branches are parsed independently. Their `**`s do not influence each other.

### 7.9 Braces and character classes

`{[ab],c}` is legal. Brace and class scopes are independent and parsed by separate sub-scanners (§9.2).

---

## 8. Path Separator and `**` Semantics

`**` is the most semantically rich construct in the dialect; it is specified in this dedicated section.

### 8.1 Globstar must own a segment

The token `**` is recognized as a `Globstar` if and only if:

- it is preceded by `/` or by the start of the pattern, AND
- it is followed by `/` or by the end of the pattern.

Otherwise, `**` **degrades to two consecutive `*`** which collapses to a single `*` (since both can match only inside a segment).

Examples:

- `src/**/*.ts` — legal globstar.
- `**/*.ts` — legal globstar (start + `/`).
- `**` — legal globstar (start + end).
- `src/**` — legal globstar (`/` + end).
- `a**b` — NOT a globstar; degrades to `a*b`.
- `a/**b` — `**` becomes `*`; equivalent to `a/*b`.
- `a**/b` — `**` becomes `*`; equivalent to `a*/b`.

At a brace-branch edge, the boundary test uses the **expanded form**
(§7.0): the effective neighbor of a branch-leading `**` is whatever
precedes the `{`, and of a branch-trailing `**` whatever follows the
matching `}` (chained through nested braces).

- `a{**,x}b` — neighbors `a`/`b` → degrades: ≡ `a{*,x}b`.
- `{**,x}/b` — pattern start / `/` → real globstar: ≡ `**/b ∪ x/b`.
- `{a/**,x}c` — trailing neighbor `c` → degrades: ≡ `a/*c ∪ xc`.
- `a/{**/x,y}` — real globstar with the same lenient `/**/` boundary
  as `a/**/x` (matches `a//x`).

### 8.2 Match semantics

A legal globstar `**` corresponds to the regex `.*` (any byte sequence, including separators, including empty).

Formally, with Σ the set of all bytes (including `/`):

```
L(**)         = Σ*                                    — any string, possibly empty
L(a/**)       = { "a/" + w : w ∈ Σ* }                 — "a/" plus anything (incl. just "a/")
L(**/b)       = { w + "b" : w ∈ Σ*, w empty or ends with / }
L(a/**/b)     = { "a/" + w + "b" : w empty or ends with / }
              = "a/b" | "a/x/b" | "a/x/y/b" | ...
```

Key invariants:

- `**` MAY match the empty string (i.e. "zero segments").
- `**` is the only construct that is allowed to cross `/`; this is its sole semantic difference from `*`.
- `a/**` requires the literal prefix `a/`. Therefore `a` (no trailing `/`) does NOT match, but `a/` (empty suffix) DOES match.

### 8.3 Behavior of `a/**` against the three forms of `a`

| Input   | `is_match` | Reason                  |
| ------- | ---------- | ----------------------- |
| `a`     | **false**  | missing the `a/` prefix |
| `a/`    | **true**   | `a/` + empty suffix     |
| `a/b`   | **true**   | `a/` + `b`              |
| `a/b/c` | **true**   | `a/` + `b/c`            |

This aligns with `picomatch`, `fast-glob`, `globset`, and the user's own `fast-glob` Rust port. We deliberately diverge from `doublestar` (Go), which hard-codes `a/**` to also match `a`.

If a caller wants the walker to yield the directory `a` itself, they SHOULD write `{a,a/**}` or use the trailing-`/` directory-only pattern form, not rely on `a/**` to mean "a and all its descendants".

### 8.4 `match_dir("a", "a/**")`

Walkers query `match_dir(d, P)` against directory paths that filesystems typically return WITHOUT a trailing `/`. The specified results:

```
match_dir("a",   "a/**") → Descend            // descendants like "a/x" may match; "a" itself does not
match_dir("a/",  "a/**") → Match              // "a/" matches itself; nothing deeper to consider
match_dir("a/b", "a/**") → DescendAndMatch    // matches now; "a/b/x" would also match
```

Implementations MAY realize this by feeding the directory path as a prefix through the matcher's NFA / DFA and classifying the resulting state set ("at an accept" vs "still has live transitions").

### 8.5 Leading `**/` ("at any depth")

When a pattern begins with `**/`, the semantics are "X at any depth", which covers:

- relative paths: `foo`, `a/foo`, `a/b/foo`;
- Unix absolute paths: `/foo`, `/a/foo`, `/a/b/foo`;
- UNC paths: `//server/foo`, `//server/share/foo`;
- Windows drive-letter paths: `C:/a/foo`, `C:\a\b\foo` (`C:` itself is a legal segment, so this works without special-casing).

Formally:

```
**/X ≡ (SEP | seg/)* X
```

where `SEP` is one separator and `seg` is a non-empty non-separator byte run. The prefix is "an optional root marker plus zero or more complete segments".

Comparison of the four `**`-related forms:

| Pattern      | Semantics                                  | `path = "/"`                     | `path = "/a"` | `path = "a/b"` |
| ------------ | ------------------------------------------ | -------------------------------- | ------------- | -------------- | --- |
| `**` (alone) | `.*`                                       | ✅                               | ✅            | ✅             |
| `**/`        | `(SEP                                      | seg/)\*`, must end at a boundary | ✅            | ❌             | ❌  |
| `**/X`       | X at any depth                             | —                                | —             | ✅ (X = `b`)   |
| `/**/X`      | X at any depth, **absolute path required** | —                                | —             | —              |

**User mental model:** `**/*.rs` means "every `.rs` file" — relative, absolute, or UNC. To restrict to absolute paths only, write `/**/*.rs` explicitly.

**Implementation note.** When the lowering pass produces `OptSegmentsSlash` as `op[0]`, it prepends a `LeadingSeps` op (matches `[/\\]*`). A `**/` in the middle of the pattern (where `op[0]` is something else such as `Lit` or `Sep`) is unaffected. When `op[0]` is an `Alternation`, the lowering recurses into each branch independently (§8.5 below).

**Per-branch recursion in top-level braces.** When the head is an `Alternation`, each branch gets the leading-seps treatment INDEPENDENTLY. For `{**/a, README}`:

- the `**/a` branch begins with `OptSegmentsSlash`, so it gets `LeadingSeps` prepended;
- the `README` branch begins with `Lit`, so it does NOT.

Consequence: `{**/a, README}` matches `/a` (via the `**/a` branch) but does NOT match `/README` (the literal branch stays strict). This matches user intent: writing a literal pattern is an explicit declaration that the absolute form is rejected.

Mid-pattern braces such as `a/{**/x,**/y}` start with `Lit` at the head, so the recursion is not triggered — matching the principle "intermediate braces do not loosen anything".

### 8.6 Consecutive `**/**/`

Normalized to a single `**/`. Semantics unchanged. This mirrors fast-glob's `skip_globstars` optimization.

### 8.7 `**` does NOT cross brace boundaries

In `{src/**, lib/**}` the two `**`s are independent — they are not merged or aliased.

### 8.8 `**` and `match_dir`

Detailed in §13. The headline: a pattern containing `**` returns `Descend` for almost every directory path, and pruning relies on the non-`**` structure surrounding it.

---

## 9. Escapes and the Leading `!`

### 9.1 Escape `\X` and meta-character usage rules

**Escape:**

- `X` is a meta-character → produces `X` as a literal byte;
- `X` is an ordinary byte → produces `X` as a literal byte (lenient);
- `\` at end-of-pattern → parse error (`TrailingBackslash`).

Examples:

- `\*` → literal `*`;
- `\\` → literal `\`;
- `\a` → literal `a` (≡ `a`);
- `foo\` → `TrailingBackslash`.

**Meta-character categories** (each decides the bare-form behavior):

| Class        | Bytes     | Bare-form rule                                                                                                                              | Literal form                         |
| ------------ | --------- | ------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------ |
| Always-meta  | `* ?`     | always meta (wildcard)                                                                                                                      | `\*` / `\?`                          |
| Context-meta | `[ ] { }` | the opener enters a syntactic scope that MUST be closed; the closer is meta only inside the corresponding open scope, **otherwise literal** | `\[` / `\]` / `\{` / `\}` (optional) |

**Context-meta details:**

- `[` always opens a class — MUST be closed by `]`, otherwise `UnterminatedClass`.
- `{` always opens a brace — MUST be closed by `}`, otherwise `UnterminatedBrace`.
- `]` and `}` are LITERAL when no corresponding opener is currently active (no error).
- `(` and `)` have NO context-meta role in this dialect (extglob is out of scope). They are literal bytes everywhere.

Examples:

- `a]b` → literal `a]b` (no `[` open, so `]` is plain).
- `{a,}}` → brace `{"a", ""}` followed by literal `}`; matches `a}` or `}`.
- `file(1).txt` → literal (no extglob trigger).
- `@(a|b)` → literal `@(a|b)`.
- `a\]b` / `a\}b` → equivalent to the bare forms above; `\` is optional.

**Why this is symmetric.** All closers (`]`, `}`) share one rule: "meta inside the corresponding opener context, literal otherwise". The MUST-close contract belongs to the openers (`[`, `{`); closers without a live opener are simply ordinary bytes.

### 9.2 Scanner independence

The parser is a single left-to-right pass. Each opener (`[`, `{`) hands control to a self-contained sub-scanner that consumes input by its own rules until it finds its own closer. The sub-scanner is OBLIVIOUS to the outer context.

Consequence: an inner closer can be "invisible" to the outer scanner.

| Pattern  | Trace                                                                                                                     | Result              |
| -------- | ------------------------------------------------------------------------------------------------------------------------- | ------------------- |
| `[{]}`   | `[` opens class, `{` is a literal class member, `]` closes; trailing `}` is literal                                       | matches `{}`        |
| `{[}]`   | `{` opens brace, `[` opens an inner class that consumes `}` as a literal member then `]` closes; brace never sees its `}` | `UnterminatedBrace` |
| `[}{]`   | `[` opens class, both `}` and `{` are literal members, `]` closes                                                         | class `{}, {{}}`    |
| `{a,[]}` | `{` opens brace, `[` opens a class, POSIX first-`]` is literal, no second `]` arrives                                     | `UnterminatedClass` |
| `[(])`   | `[` opens class, `(` literal, `]` closes, trailing `)` is literal                                                         | matches `()`        |

The asymmetry is NOT about "which opener has higher priority" but about "whoever opens FIRST owns the input". A class scanner does not know it is inside a brace, so it consumes `}` as a literal class member per §6.3.

This is the same model as bash brace expansion, POSIX bracket expressions, and regex `(...[)]...)`. The alternative ("brace scanner protects its own `}`") would make `[}]` context-dependent and complicate the class grammar.

In practice, patterns like `{[}]` that are "visually balanced but structurally crossed" almost always indicate typos. Failing fast is safer than bash's "degrade to literal" recovery.

### 9.3 Leading `!`

A run of `!` at the start of a pattern is whole-pattern negation:

- `!pattern` → `match := !match(pattern)`;
- `!!pattern` → double negation = `match(pattern)`;
- `!!!pattern` → triple negation = `!match(pattern)`.

General rule: an odd-count leading-`!` run negates; an even-count run cancels.

The leading `!` is a **whole-pattern** operation that flips the body's match result. There is NO segment-level negation syntax. Any `!` byte that is not part of the leading run is literal.

- `!foo` → one negation + body `foo`.
- `!(foo)` → one negation + body `(foo)` (literal). Matches every path that is not literally `(foo)`.

### 9.4 Literal leading `!`

To match a filename whose first byte is `!`, escape it: `\!foo` matches the literal `!foo`. (Or use a class: `[!]foo` ≡ `[\!]foo` matches the literal `!foo` as well.)

### 9.5 Counting the leading `!`

The parser logic:

1. Scan a contiguous run of `!` bytes at position 0.
2. An odd count produces an outer negation; an even count cancels.
3. Parsing of `Sequence` begins at the first non-`!` byte. Any later `!` is literal.

Examples:

- `!foo` → 1 negation + body `foo`.
- `!!foo` → 2 negations (cancel) + body `foo`.
- `!(foo)` → 1 negation + body `(foo)` (literal).
- `!!(foo)` → cancels + body `(foo)`.
- `!!!(foo)` → 1 effective negation + body `(foo)`.

### 9.6 Interaction with options

The leading `!` is always honored. There is no toggle that disables it, because it is a syntactic feature of the pattern, not a matching behavior.

---

## 10. Compile-time Errors

The parser MUST return an error for the following inputs:

| Error                 | Example                    | Reason                                   |
| --------------------- | -------------------------- | ---------------------------------------- |
| `Empty`               | `""`                       | no defined semantics                     |
| `TooLong`             | `len > 64 KiB`             | DoS guard                                |
| `UnterminatedClass`   | `[abc`                     | opener requires its closer               |
| `UnterminatedBrace`   | `{a,b`                     | opener requires its closer               |
| `TrailingBackslash`   | `foo\`                     | typo                                     |
| `BraceNestingTooDeep` | `{{{{...}}}}` (>32 levels) | DoS guard                                |
| `InvalidRange`        | `[z-a]`                    | upper < lower                            |
| `EmptyPatternSet`     | `globstar([])`             | the multi-pattern factory needs ≥1 input |

> Note: stray `]`, `}`, `)`, `(` are NOT errors — see §9.1, they degrade to literal bytes under the context-meta rule. `(` and `)` have no opener role in this dialect, so there is no "unclosed paren" concept.

The parser SHOULD be lenient (produce a sensible default rather than an error) for these:

| Lenient case              | Example     | Output                                                           |
| ------------------------- | ----------- | ---------------------------------------------------------------- |
| `\X` where X is ordinary  | `\a`        | literal `a`                                                      |
| `-` at class end          | `[a-]`      | `a` or `-`                                                       |
| `-` at class start        | `[-a]`      | `-` or `a`                                                       |
| Single-branch brace       | `{a}`       | literal `{a}`                                                    |
| `**` not owning a segment | `a**b`      | equivalent to `a*b`                                              |
| Consecutive `**/**`       | `a/**/**/b` | normalized to `a/**/b`                                           |
| Repeated `/`              | `a//b`      | unchanged at parse time, but `**` boundaries lenient (see §12.3) |

Reference error type:

```rust
// Rust crate
pub enum GlobError {
    Empty,
    TooLong { len: usize, max: usize },
    UnterminatedClass { at: usize },
    UnterminatedBrace { at: usize },
    TrailingBackslash,
    BraceNestingTooDeep { max: usize },
    InvalidRange { at: usize, low: u8, high: u8 },
    EmptyPatternSet,
}
```

```ts
// JS package — same shape, conveyed via the .kind tag on GlobError
class GlobError extends Error {
  readonly kind:
    | "Empty"
    | "TooLong"
    | "UnterminatedClass"
    | "UnterminatedBrace"
    | "TrailingBackslash"
    | "BraceNestingTooDeep"
    | "InvalidRange"
    | "EmptyPatternSet";
}
```

> **Historical note.** Earlier drafts had `EmptyClass` and `EmptyNegationClass` covering `[]`, `[!]`, `[^]`. After the POSIX first-`]` rule was adopted (§6.5), all three cases collapsed into `UnterminatedClass`. Those variants have been removed.

---

## 11. Options

There are exactly two matcher-semantics options. All other syntactic constructs (brace, globstar, ...) are always enabled and offer no toggle.

```rust
// Rust
pub struct CompileOptions {
    pub dot: bool,
    pub case_insensitive: bool,
}
```

```ts
// JS
interface GlobstarOptions {
  dot?: boolean;
  caseInsensitive?: boolean;
}
```

### 11.1 `dot`

Controls "Bash-style dot protection": at every segment-start position (including position 0 of the path), are `*`, `?`, and `[^...]` allowed to consume `.`?

**Default at the matcher layer: `true`.** The matcher is a pure pattern↔path string predicate; there is no filesystem "hidden file" convention to honor at this layer. The walker layer overrides the default (`false`); see WALKER_SPEC.md §3 for the rationale.

Precise positional semantics in §12.4. Top-level summary:

`dot = false` (walker default, matches bash / picomatch):

- `*.txt` matches `a.txt`, NOT `.txt`.
- `**/*` matches `a/b`, NOT `a/.b`.
- To match a dotfile: write a literal-`.` pattern (`.*`, `[.]*`) or pass `dot: true`.

`dot = true` (matcher default): all dot protection is disabled. `*.txt` matches `.txt`, `**/*` matches `a/.b`.

**Why split the default by layer.** Dot protection is a filesystem convention, not a property of the pattern language itself. Putting the "hide-dotfile" default at the walker layer keeps the pattern language pure and matches ecosystem expectations for filesystem traversal.

### 11.2 `case_insensitive`

When `case_insensitive = true`, **ASCII letters** (`A-Z`, `a-z`) are case-folded during byte comparison: `*.ts` matches `MAIN.TS`; `[A-Z]` is equivalent to `[A-Za-z]`.

**Key principle: case insensitivity is a matching-time behavior, not a syntactic predicate.** Specifically:

- The parser does NOT inspect the `case_insensitive` flag.
- All syntactic validity checks (range validity, unterminated brackets, `/` inside a class) are byte-level and case-sensitive.
- `[a-A]` is ALWAYS `InvalidRange` (`a` = 97 > `A` = 65), even with `case_insensitive = true`. Validity is a syntactic constraint; case-folding only affects byte comparison at match time.
- `[/]` is always `UnterminatedClass` (§6.2); `[]` is always `UnterminatedClass` (§6.5). The flag does not change either.

Keeping these orthogonal lets the parser have a single compilation path and produces error messages that always match the user's literal input.

**Scope: ASCII only** (§12.5). Bytes outside ASCII (UTF-8 multibyte sequences, Latin-1, etc.) are compared verbatim, with NO Unicode case folding:

- `café.txt` does NOT match `CAFÉ.txt` (`É` is non-ASCII).
- `café.txt` matches `CAFé.txt` (`É` is identical in both sides; only ASCII letters fold).

Callers needing Unicode case folding MUST normalize both pattern and path before passing them in (e.g. NFKC + lowercase via the `unicode-normalization` crate or equivalent).

**Implementation strategy.** Case folding happens during **lowering**, not parsing — runtime is branchless:

- Class items are expanded at lower time: `[A-Z]` gets a `[a-z]` Range added (the parser already validated it as a legal range).
- `Op::Lit` bytes stay as-is, but the comparator becomes `eq_ignore_ascii_case` (single-byte fold, ~1 ns).
- The Glushkov / Thompson DFA byte-class table includes the case-alt byte alongside each literal.
- The `LiteralFacts` prefilter stores raw bytes and does the fold at comparison time; the `case_insensitive` flag is propagated.

**Default:** `false` (strict byte-level match) at both the matcher and walker layers. Callers MUST opt in explicitly.

> See WALKER_SPEC.md §5 for the case-insensitive caveat that affects the walker's `static_prefixes` seeding optimization.

### 11.3 Toggles deliberately NOT provided

| Considered        | Status          | Rationale                                                  |
| ----------------- | --------------- | ---------------------------------------------------------- |
| `brace_expansion` | always on       | use `\{a,b\}` to disable for one literal                   |
| `globstar`        | always on       | no real-world need for a toggle                            |
| `unicode_case`    | not implemented | callers normalize first; we do not bear Unicode complexity |

Rationale: each toggle would require parser state, polluting the main path, while the disable case represents <1% of real users and is always expressible via escapes.

---

## 12. Match Semantics

### 12.1 Definition of "match"

For a pattern P and a path:

```
is_match(P, path) := path ∈ L(P)
```

where L(P) is the language defined by P.

### 12.2 Whole-path matching

Matches are ALWAYS whole-path: the entire path bytes MUST be consumed by P. There is no substring-style matching (bash, grep).

### 12.3 Path separator handling

**Pattern layer.** `/` is the only segment delimiter, regardless of platform. `\` is always the escape character (§9.1) — never a separator. All structural semantics (segment ownership, `**` segment crossing, class segment-locality, dot-protection trigger) are evaluated against `/` in the pattern.

**Path layer.** To accept platform-native paths, the matcher recognizes an **implementation-defined separator byte set `Seps`**:

- `/` ∈ `Seps` is a spec mandate.
- Other bytes (typically `\`) are at the implementation's discretion and conventionally follow platform norms.
- Reference implementation: `Seps := { b : std::path::is_separator(b as char) }`. `/` is always in. `\` is in on Windows, out on every other platform.

`Seps` membership influences these structural decisions (all "is this a separator?" predicates route through `Seps`):

- `Sep` / `**/` / `LeadingSeps` ops accept any `Seps` byte.
- Segment-start dot-protection trigger: "previous byte ∈ `Seps`".
- Class segment-local guard: `[...]` and `[^...]` reject any `Seps` byte (§6.2). The parser already rejects explicit `/`; the runtime guard handles platform-conditional bytes such as `\`.

**Strict vs lenient separator consumption.** Each `/` in a pattern compiles to a STRICT `Sep` op that consumes EXACTLY ONE `Seps` byte from the path. Therefore `a/b` does NOT match `a//b`. Patterns that contain `//` are semantically distinct from those that contain a single `/`: `a//b` requires exactly two separator bytes in the path.

Lenient ("1+ separator runs") consumption appears in exactly two places:

- **`**` boundaries** (`/**/`): in the globstar fold, the `/`adjacent to`**`is upgraded from`Sep`to`SepRun`(1+). So`a/\*\*/b`does match`a//b`. This matches picomatch / globset / wax.
- **Pattern-leading `**/`**: the lowering pass prepends a `LeadingSeps`op (0+) so`\*\*/foo`matches both relative paths and`/foo`, `//server/share/foo`, ... (§8.5).

**Cross-platform consequences.** Different implementations, or the same implementation on different platforms, MAY return different match results for the same (pattern, path) pair. This is the price of accepting native paths and is an acknowledged trade-off. Callers that require strict portability SHOULD normalize paths to `/`-only form before passing them in.

**What the matcher does NOT do:**

1. Byte substitution — `\` is not "normalized" to `/`; it is treated as a separator only at the predicate level.
2. Pattern `//` collapsing — each `/` in a pattern is its own `Sep` op.
3. Path `//` collapsing — `Sep` is strict; only `SepRun` and `LeadingSeps` absorb runs.
4. `.` / `..` resolution — caller's responsibility; matched as bytes.
5. Symlink resolution — walker layer's responsibility.

### 12.4 Precise dot-protection semantics

When `dot = false`, a path position is **dot-protected** if and only if:

- position 0 AND `path[0] == '.'`; OR
- position `i > 0` AND `path[i-1]` ∈ `Seps` AND `path[i] == '.'`.

At a dot-protected position:

- `*` MUST NOT consume the `.` (it MAY consume zero characters);
- `?` MUST NOT match the `.`;
- `[^x]` MUST NOT match the `.`;
- a literal `.`, the class `[.]`, or any class with `.` as an explicit member, matches normally;
- `**` is unaffected directly, but every segment-start position that `**` exposes is itself subject to the rule.

### 12.5 Multi-byte characters

Match by byte. `?` consumes ONE byte, NOT ONE Unicode codepoint.

If both pattern and path are well-formed UTF-8 and the user wants `?` to match "one CJK character" (3 bytes in UTF-8), they MUST write `???`. This is an acknowledged non-goal.

---

## 13. Directory Matching `match_dir`

### 13.1 Definition

For a pattern P and a directory path `d`, `match_dir(P, d)` returns exactly one of:

```rust
enum DirMatch {
    Pruned,           // No string in L(P) is prefixed by d → entire subtree is hopeless
    Descend,          // d is a strict prefix of some string in L(P)
    Match,            // d ∈ L(P), but no descendant of d is in L(P)
    DescendAndMatch,  // d ∈ L(P), AND some descendants of d are also in L(P) (typically via `**`)
}
```

### 13.2 Semantics

Define

```
L_prefix(P) = { w : ∃ suffix.  w ++ suffix ∈ L(P), with w empty or ending in / }
```

Then

```
match_dir(P, d) :=
  let s = if d ends with '/' then d else d ++ "/"   // canonicalize
  match (s ∈ L(P), s ∈ L_prefix(P)):
    (true,  true)  → DescendAndMatch
    (true,  false) → Match
    (false, true)  → Descend
    (false, false) → Pruned
```

### 13.3 Examples

| Pattern             | Dir                              | Result          | Reason                             |
| ------------------- | -------------------------------- | --------------- | ---------------------------------- |
| `src/**/*.ts`       | `src`                            | Descend         | descendants may have `.ts`         |
| `src/**/*.ts`       | `src/components`                 | Descend         | same                               |
| `src/**/*.ts`       | `tests`                          | Pruned          | not under `src`                    |
| `src/**/*.ts`       | `node_modules`                   | Pruned          | the critical pruning case          |
| `src/**/*.ts`       | `src/main.ts` (queried as a dir) | DescendAndMatch | matches now and could match deeper |
| `src/components`    | `src`                            | Descend         |                                    |
| `src/components`    | `src/components`                 | Match           | exact, no descendant continues     |
| `src/components/**` | `src/components`                 | Descend         | `**` requires ≥1 segment           |
| `src/components/**` | `src/components/a`               | DescendAndMatch | `**` allows any depth              |
| `{src,lib}/**`      | `src`                            | Descend         |                                    |
| `{src,lib}/**`      | `docs`                           | Pruned          |                                    |
| `**/*.ts`           | `any/dir`                        | Descend         | `**` allows any depth              |

### 13.4 Leading `!` and `match_dir`

Whole-pattern negation `!P` would naively yield `match_dir(!P, d) = invert(match_dir(P, d))`. In practice this gives almost no pruning information: a negated pattern normally means "all paths NOT matching X", and a consumer (typically a filesystem walker) must enumerate the whole tree to find them.

**Specified behavior:** `match_dir(!P, d)` SHOULD conservatively return `Descend`, except when the entire subtree is provably matching (a rare corner case). The matcher's multi-pattern factory (§14.1) implements exactly this — see the `matchDir` body in `matcher/glob.js` for the JS reference.

### 13.5 Implementation hints

1. Glushkov NFA / Thompson NFA: feed `d` as bytes to the state machine, classify by the resulting active state set:
   - empty → `Pruned`,
   - contains an accept state → `Match` or `DescendAndMatch`,
   - non-empty and no accept → `Descend`.
2. Derivative DFA: run transitions to the end of `d`, classify the destination state.
3. Backtracking matcher: needs special treatment — run once to the end, then check whether any "partial match continuation" state exists.

---

## 14. Multi-pattern Matchers

### 14.1 Definition

A multi-pattern matcher combines N globs with boolean OR. In Rust this is `GlobSet`; in JS it is `globstar(patterns: string[])`.

```rust
// Rust
pub struct GlobSet { globs: Vec<Glob> }
```

```ts
// JS
function globstar(patterns: string | readonly string[], opts?): (input) => boolean;
```

For a path, `globset.matches(path)` returns the set of glob indices that match. A non-empty set means "at least one glob matched".

### 14.2 Relationship to a single glob

```
GlobSet::is_match(path) ≡ ∃ g ∈ globs.  g.is_match(path)
```

It is a "boolean OR", NOT "boolean AND".

### 14.3 Combined `match_dir`

```
GlobSet::match_dir(d) :=
  let results   = [ g.match_dir(d) for g in globs ]
  let any_match = results.any(contains Match)
  let any_desc  = results.any(contains Descend)
  combine(any_match, any_desc)
```

Headline: **if any glob says `Descend`, the combined result MUST be `Descend`**. Pruning is only safe when EVERY glob says `Pruned`.

### 14.4 Literal acceleration

Implementations MAY build a literal-prefix accelerator (e.g. an Aho-Corasick automaton over per-glob suffix or substring facts). The accelerator filters candidate glob ids before running the precise matcher on each candidate. See `theory/06-aho-corasick.md` and `theory/07-literal-acceleration.md` (Rust crate) for the construction.

---

## 15. Edge Case Catalog

This section is the canonical preview of `tests/corpus/*.txt`. Each line below corresponds to one or more rows in the corpus.

### 15.1 Basic wildcards

```
*              path=""          ⇒ true (empty path)
*              path="a"         ⇒ true
*              path="/"         ⇒ false
*              path="a/b"       ⇒ false
*.txt          path=".txt"      ⇒ false  (dot=false default)
*.txt          path="a.txt"     ⇒ true
?.txt          path="a.txt"     ⇒ true
?.txt          path=".txt"      ⇒ false  (dot=false)
```

### 15.2 `**`

```
**             path=""          ⇒ true
**             path="a"         ⇒ true
**             path="a/b/c"     ⇒ true
a/**           path="a"         ⇒ false
a/**           path="a/b"       ⇒ true
a/**           path="a/b/c"     ⇒ true
a/**/b         path="a/b"       ⇒ true
a/**/b         path="a/x/b"     ⇒ true
a/**/b         path="a/x/y/b"   ⇒ true
**/b           path="b"         ⇒ true
**/b           path="a/b"       ⇒ true
**/**/b        path="a/b"       ⇒ true   (consecutive `**` collapse)
```

### 15.3 `**` degradation

```
a**b           path="ab"        ⇒ true   (degrades to a*b)
a**b           path="axb"       ⇒ true
a**b           path="a/b"       ⇒ false  (single segment)
```

### 15.4 Character classes

```
[abc]          path="a"         ⇒ true
[abc]          path="d"         ⇒ false
[a-z]          path="x"         ⇒ true
[a-z]          path="X"         ⇒ false  (case-sensitive default)
[!abc]         path="d"         ⇒ true
[^abc]         path="d"         ⇒ true
[!abc]         path="a"         ⇒ false
```

### 15.5 Brace

```
{a,b}          path="a"         ⇒ true
{a,b}          path="b"         ⇒ true
{a,b}          path="c"         ⇒ false
{a,}           path="a"         ⇒ true
{a,}           path=""          ⇒ true
{a,{b,c}}      path="b"         ⇒ true
{a,b}{c,d}     path="ac"        ⇒ true
{a,b}{c,d}     path="bd"        ⇒ true
{a,b}{c,d}     path="ad"        ⇒ true
{a}            path="{a}"       ⇒ true   (single-branch is literal)
```

### 15.6 Escapes

```
\*             path="*"         ⇒ true
\*             path="a"         ⇒ false
\\             path="\"         ⇒ true
\a             path="a"         ⇒ true   (lenient)
foo\           parse error                (TrailingBackslash)
```

### 15.7 Leading negation

```
!foo           path="foo"       ⇒ false
!foo           path="bar"       ⇒ true
!!foo          path="foo"       ⇒ true
!!!foo         path="foo"       ⇒ false
\!foo          path="!foo"      ⇒ true
```

### 15.8 Dot protection

```
*              path=".hidden"   ⇒ false  (dot=false)
.*             path=".hidden"   ⇒ true
*              path=".hidden"   ⇒ true   (dot=true)
**/*           path="a/.b"      ⇒ false  (dot=false)
**/*           path="a/.b"      ⇒ true   (dot=true)
```

### 15.9 Directory matching (`match_dir`)

```
src/**/*.ts    dir="src"               ⇒ Descend
src/**/*.ts    dir="src/components"    ⇒ Descend
src/**/*.ts    dir="tests"             ⇒ Pruned
src/**/*.ts    dir="node_modules"      ⇒ Pruned
{src,lib}/**   dir="src"               ⇒ Descend
{src,lib}/**   dir="lib"               ⇒ Descend
{src,lib}/**   dir="docs"              ⇒ Pruned
**/*.ts        dir="anything"          ⇒ Descend
```

### 15.10 Error cases

```
""             ⇒ Err(Empty)
"["            ⇒ Err(UnterminatedClass)
"[]"           ⇒ Err(UnterminatedClass)   # POSIX first-`]` is literal, no closer
"[!]"          ⇒ Err(UnterminatedClass)
"[]]"          ⇒ Ok — class {`]`}
"[]-]"         ⇒ Ok — class {`]`, `-`}
"{a,b"         ⇒ Err(UnterminatedBrace)
"foo\\"        ⇒ Err(TrailingBackslash)
"[z-a]"        ⇒ Err(InvalidRange)
```

The POSIX first-`]` rule (consistent with bash / fnmatch / fast-glob / picomatch): when `]` appears immediately after `[`, `[!`, or `[^`, it is a literal class member, not a closer. `[]` therefore degrades to "literal `]` with no terminator" → `UnterminatedClass`. The earlier `EmptyClass` variant is no longer produced.

---

## 16. Differences from Other Tools

| Behavior                       | Us                                  | Bash               | picomatch       | globset                    | fast-glob                  | doublestar    |
| ------------------------------ | ----------------------------------- | ------------------ | --------------- | -------------------------- | -------------------------- | ------------- |
| `dot` default                  | `true` (matcher) / `false` (walker) | `false`            | `false`         | `true` (no concept)        | `true` (no concept)        | `false`       |
| `case_insensitive`             | ASCII-only, default `false`         | `shopt nocaseglob` | `nocase` option | `case_insensitive` builder | `caseSensitiveMatch=false` | Unicode-aware |
| `**` must own a segment        | enforced                            | ✅                 | ✅              | ✅                         | ✅                         | ✅            |
| `a/**` matches `a`             | ❌                                  | ⚠️                 | ⚠️              | ❌                         | ❌                         | ❌            |
| `**` in braces judged on expansion (§7.0: `a{**,x}b` ≡ `a{*,x}b`) | ✅ | ✅ (expands first) | ❌ (branch-local `.*`) | ❌ (demotes to `*` even at boundaries) | ❌ | n/a |
| `**/X` matches absolute `/a/X` | ✅ (§8.5)                           | n/a                | ✅              | ✅                         | ✅                         | ❌            |
| Extglobs                       | ❌ (literal)                        | ✅ (`shopt`)       | ✅              | ❌                         | ❌                         | ✅            |
| POSIX character classes        | ❌                                  | ✅                 | ✅              | ❌                         | ❌                         | ⚠️            |
| `[/]` inside a class           | parse error (§6.2)                  | literal            | literal         | literal                    | literal                    | ⚠️            |
| POSIX first-`]` rule           | ✅ (§6.5)                           | ✅                 | ✅              | ❌                         | ❌                         | ⚠️            |
| Brace `{1..10}`                | ❌                                  | ✅                 | ✅              | ❌                         | ❌                         | ❌            |
| Brace nesting                  | ≤ 32                                | ✅                 | ✅              | ✅                         | ≤ 10                       | ✅            |
| Leading-`!` negation           | ✅                                  | n/a                | ✅              | ⚠️                         | ✅                         | ✅            |
| `/` required in patterns       | ✅                                  | n/a                | ⚠️              | ⚠️                         | ⚠️                         | ⚠️            |
| `\` is always escape           | ✅                                  | ✅                 | ✅              | ⚠️                         | ⚠️                         | ⚠️            |
| `\` in path = sep              | platform-conditional (§12.3)        | ❌                 | ✅ (JS paths)   | platform                   | ❌                         | platform      |
| Stray `]`, `}`, `)`            | literal (§9.1)                      | literal            | literal         | literal                    | literal                    | literal       |
| Unclosed constructs            | parse error                         | literal            | ⚠️              | error                      | error                      | literal       |
| Trailing `/` = directory       | ✅                                  | ⚠️                 | ⚠️              | ❌                         | ❌                         | ⚠️            |
| `match_dir` API                | ✅ (first-class)                    | n/a                | ❌              | ✅                         | ❌                         | ❌            |

**Reading.** Our spec is closest to picomatch + doublestar. It is more strict on **input conventions** (mandatory `/` in patterns), more explicit on **`match_dir` as a first-class operation**, and more rigorous on **syntax-level errors** (unterminated classes / braces are hard errors, never silently degraded).

---

## 17. Appendix A: Version History

- **v0.1** (2026-04-11): initial draft, based on D-001 / D-004 / D-005 consensus and `semantics-table.md`.
- **v0.1.1** (2026-04-11): rewrote §8 against D-009 and the user's `fast-glob` `tests/test.rs` evidence.
  - `L(**)` changed from "zero or more segments" to "any byte string" (regex `.*`).
  - `L(a/**)` clarified as `{ "a/" + w : w ∈ Σ* }`, including `a/` (empty suffix) but excluding `a`.
  - Added §8.4: precise `match_dir` behavior for `a/**`.
  - §15.2 corrected `a/**` vs `a/` from no-match to match.
- **v0.1.2** (2026-04-14): batch of semantic refinements driven by fast-glob corpus import.
  - §6.2: class `/` changed from silent-ignore to `UnterminatedClass`. `\` retained as the pattern escape character (runtime short-circuits handle the platform-conditional separator case).
  - §6.5: POSIX first-`]` rule introduced. `[]`, `[!]`, `[^]` produce `UnterminatedClass`. `[]]`, `[!]]`, `[]-]` are legal. `GlobError::EmptyClass` and `EmptyNegationClass` removed.
  - §8.5 (renumbered): added `**/X` "any depth" semantics, implemented via a `LeadingSeps` op that absorbs an optional root separator run. Top-level brace per-branch recursion lets only the `**/`-headed branches receive the relaxation.
- **v0.1.3** (2026-04-14): cross-platform separator semantics.
  - §12.3: introduced **implementation-defined separator byte set `Seps`** (`/` ∈ `Seps` mandate; other bytes implementation-chosen).
  - §6.2: class `Seps` short-circuit unified across positive and negative classes.
- **v0.1.4** (2026-04-14): `dot` default split by layer.
  - Matcher default flipped to `true`. Walker stays at `false`.
- **v0.1.5** (2026-04-14): class `Seps` guard unified across both polarities (positive class also short-circuits on `Seps` members).
- **v0.1.6** (2026-04-14, **rolled back**): briefly tried paired-strict (stray `]`/`}` as errors). Reverted in favor of the symmetric-loose rule because real filename matching needs `]`/`}` as plain bytes more often than typo detection helps.
- **v0.1.7** (2026-04-14): ASCII `case_insensitive` matcher option added. Compile-time class fold; runtime branchless. Walker `static_prefixes` retains a documented case-insensitive caveat.
- **v0.2.0** (2026-04-23): bash extglob removed. `@()`, `?()`, `*()`, `+()`, `!(...)` are pure literal byte sequences. Grammar / error enum / engine layering all slimmed accordingly. See ADR-003 (Superseded).
- **v0.2.1** (2026-07-23): braces defined by the expansion equation (§7.0).
  `**` segment ownership is judged on the expanded form — `a{**,x}b`
  now degrades its `**` (previously a branch-local `.*` that crossed
  separators, matching picomatch), `{**,x}/b` now matches `b`, and
  `a/{**/x,y}` gains `/**/`-boundary leniency. Aligns with bash /
  minimatch; deliberate divergence from picomatch recorded in §16.
  Corpus group `brace.globstar-expansion` pins the behavior.
- Awaiting owner approval before freezing as v1.0.

---

## 18. Appendix B: Glossary

- **Pattern.** The user-supplied glob string.
- **Path / path bytes.** The byte sequence being matched.
- **Segment.** A `/`-delimited piece of the path.
- **`Seps`.** The implementation-defined set of separator bytes (§12.3). `/` ∈ `Seps` is a spec mandate; the reference implementation defines `Seps := { b : std::path::is_separator(b as char) }`.
- **Dot-protected.** A position whose first byte is `.` and whose preceding byte is a separator (or is the start of the path). Default `*`/`?` cannot consume a `.` at such positions.
- **`Match` / `Pruned` / `Descend` / `DescendAndMatch`.** The four results of `match_dir` (§13).
- **GlobSet (Rust) / multi-pattern matcher (JS).** The boolean-OR combination of N globs. `matches` returns the set of matching glob ids.
- **Literal facts.** The set of guaranteed literal byte facts extracted from a pattern (prefix, suffix, contains, exact). Used by the prefilter.
- **Static prefixes.** The set of byte strings that every matching path of a pattern MUST be prefixed by. Used by walker traversal to jump to the deepest known directory before walking.
