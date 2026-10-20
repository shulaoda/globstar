# GLOB_SPEC: Syntax and Semantics

**Status.** This document is the normative definition of the glob dialect implemented by the `globstar` Rust crate (`crates/globstar`) and the `@globstar/core` JS package (`packages/globstar`). When code, tests or other documents disagree with it, the code changes.

**Baseline.** The dialect follows `picomatch` and `fast-glob` mainline behavior with the rigor of `doublestar`. Bit-exact parity with any single tool is not a goal; Appendix A lists every deliberate difference.

**Conventions.** The key words MUST, MUST NOT, SHOULD and MAY are to be read as described in RFC 2119. They state requirements on an implementation or on a caller. Sentences without a key word define the pattern language itself. A paragraph marked *Note*, *Rationale* or *Implementation note* is informative. Examples are informative.

**Conformance.** The executable form of this document is the corpus in `crates/globstar/tests/corpus/` (one row per pattern, path and expected answer; its README states the format) and the fixtures in `fixtures/`. Both runtimes run every row; a behavior counts as specified only when a row pins it.

## Table of Contents

1. [Scope](#1-scope)
2. [Model](#2-model)
3. [Options](#3-options)
4. [Lexical Rules](#4-lexical-rules)
5. [Grammar and Normalization](#5-grammar-and-normalization)
6. [Errors and Limits](#6-errors-and-limits)
7. [Literals, `?`, `*` and `/`](#7-literals---and-)
8. [Character Classes](#8-character-classes)
9. [The Globstar `**`](#9-the-globstar-)
10. [Braces](#10-braces)
11. [Negation](#11-negation)
12. [Dot Protection](#12-dot-protection)
13. [Case Folding](#13-case-folding)
14. [Matching a Path](#14-matching-a-path)
15. [Directory Matching `match_dir`](#15-directory-matching-match_dir)
16. [Unions](#16-unions)
17. [Static Prefixes](#17-static-prefixes)

- [Appendix A: Differences from Other Tools](#appendix-a-differences-from-other-tools)
- [Appendix B: Glossary](#appendix-b-glossary)

---

## 1. Scope

### 1.1 In scope

- **The pattern language:** `*`, `**`, `?`, character classes `[...]`, braces `{...}`, the escape `\`, and leading-`!` negation.
- **Matching:** what `is_match(pattern, path)` returns for every pair of inputs (§2–§14).
- **Directory matching:** the `match_dir` operation and its six results (§15). Walkers and watchers consume it; this document defines only what each result means.
- **Unions:** the boolean OR of several patterns (§16): `Glob::union` in Rust, `globstar(patterns)` and `compileMatcher(patterns)` in JS.
- **Static prefixes:** the literal heads every match of a pattern starts with (§17).

Filesystem traversal is the subject of [WALKER_SPEC.md](./WALKER_SPEC.md): cwd handling, the auto-split of `!` patterns into an ignore set, traversal order and walker errors.

### 1.2 Out of scope

These constructs are not part of the language. Where their syntax can appear in a pattern, the bytes are ordinary literals.

- **Bash extglobs** `@()`, `?()`, `*()`, `+()`, `!(...)`: `(`, `)`, `@`, `+` and `|` are literal bytes, and a `?` or `*` inside keeps its wildcard meaning, so `@(a|b)` matches only the path `@(a|b)`. For alternation use a brace (§10); for negation the leading `!` (§11).
- **POSIX character classes** `[[:alpha:]]`, collating elements `[[.a.]]` and equivalence classes `[[=a=]]`: §8.7 says how such text parses.
- **Bash brace ranges** `{1..10}`, `{a..z}`.
- **Regex features:** backreferences, groups, lookaround.
- **Gitignore re-include** ("a later rule cancels an earlier one"): a walker-layer concern.
- **Unicode case folding:** only ASCII folding is offered (§13).
- **Shell expansions:** `~`, `$VAR`.

### 1.3 Compatibility

The closest reference is `picomatch`. Every deliberate divergence is measured and listed in Appendix A.

---

## 2. Model

Everything below rests on four definitions: the bytes a pattern and a path are made of, which of those bytes separate segments, what a segment is, and what a pattern denotes.

### 2.1 Bytes

Matching is defined over bytes.

- **Patterns** are Unicode strings (`&str` in Rust, JS strings), read as their UTF-8 bytes. Non-UTF-8 pattern bytes are not accepted: a JS pattern with a lone surrogate, which has no UTF-8 form, throws a `TypeError` (a Rust `&str` cannot hold one).
- **Paths** are arbitrary bytes in Rust (`is_match(&[u8])`), so non-UTF-8 file names match byte for byte. In JS a path is a string, encoded to UTF-8 internally; a lone surrogate encodes as U+FFFD. A non-string JS input throws a `TypeError`.
- Only ASCII bytes can carry a meta meaning. Every byte outside ASCII is literal.
- Code point boundaries play no part: comparison is byte for byte, and every construct consumes bytes, not characters (§14.3).

### 2.2 Separators

**In a pattern**, `/` is the only path separator, on every platform. `\` is the escape character (§4.2), never a separator.

| Pattern        | Separator? | Meaning                                        |
| -------------- | ---------- | ---------------------------------------------- |
| `src/main.rs`  | yes        | two segments                                   |
| `src\main.rs`  | no         | one segment, `srcmain.rs`; the same on Windows |
| `src\\main.rs` | no         | one segment, `src\main.rs` with a literal `\`  |

A tool that accepts native-shell paths as patterns MUST normalize them to `/` first.

**In a path**, a separator is any byte of an implementation-defined set `Seps`:

- `/` ∈ `Seps` on every platform; this document mandates it.
- Other bytes (typically `\`) are the implementation's choice and SHOULD follow platform norms.
- The reference implementations define `Seps := { b : std::path::is_separator(b as char) }`: `\` is a separator on Windows and nowhere else.

Wherever this document asks whether a path byte is a separator, it means membership in `Seps`: when a pattern `/` consumes a byte (§7.4), when `**` crosses segments (§9), when a segment start is found (§2.3), and when a class refuses a separator (§8.2). §14.2 collects the consequences.

### 2.3 Segments

A path is a sequence of **segments** joined by single separator bytes. A segment is a (possibly empty) run of non-separator bytes. Position `i` of a path is a **segment start** when `i = 0` or `path[i-1]` ∈ `Seps`. A segment is **dot-led** when its first byte is `.`.

A pattern is likewise read as segments between its `/` bytes. Every construct but the globstar (§9) stays inside one segment: `*`, `?` and a class never consume a separator, and a brace branch may hold a `/` only as a pattern separator of its own.

### 2.4 What a pattern denotes

A pattern `P` denotes a set of paths, its language `L(P)`, and

```
is_match(P, path)  :=  path ∈ L(P)
```

A match covers the whole path: every byte of the path is consumed by `P`, and nothing is matched as a substring (§14.1). `L(P)` depends on the two options of §3; this document writes `L(P)` for the language under the options in force.

The language is built compositionally: each construct (§7–§10) denotes a set of byte strings, a sequence denotes their concatenation, the leading `!` (§11) complements, and two cross-cutting rules, dot protection (§12) and case folding (§13), restrict or widen what the wildcards consume. Appendix B restates the vocabulary.

---

## 3. Options

There are exactly two options, and both are parameters of matching, not of syntax: the parser never consults them.

```rust
// Rust; Default: dot = true, case_insensitive = false
pub struct CompileOptions {
    pub dot: bool,
    pub case_insensitive: bool,
}
```

```ts
// JS
interface GlobstarOptions {
  dot?: boolean; // default true
  caseInsensitive?: boolean; // default false
}
```

### 3.1 `dot`

Whether a wildcard may consume a `.` that starts a segment: bash-style dot protection, defined in §12. The default at the matcher layer is `true`; the walker layer defaults to `false` (WALKER_SPEC.md §3).

| Setting       | `*.txt` on `.txt` | `**/*` on `a/.b` | To match a dotfile                               |
| ------------- | ----------------- | ---------------- | ------------------------------------------------ |
| `dot = false` | no match          | no match         | name the dot (`.*`, `[.]*`) or pass `dot = true` |
| `dot = true`  | match             | match            |                                                  |

`dot = false` is the picomatch behavior. Bash and POSIX `fnmatch` are stricter: a class never matches a leading dot there (`[.]env` does not match `.env`); only a literal `.` does.

Under `dot = true`, `*` and `**` also match the names `.` and `..`: `public/**` matches `public/../secret.txt`, which picomatch never matches. A path is matched as the bytes given, never resolved (§14.2), so a caller matching paths from outside normalizes them first. The walker never meets such names, since `readdir` does not return them.

> **Rationale.** The matcher is a pure pattern-to-path predicate; hidden files are a filesystem convention, not a property of the pattern language. The convention therefore lives in the walker's default.

### 3.2 `case_insensitive`

Whether ASCII letters compare case-folded, defined in §13. The default is `false` at both layers.

### 3.3 Toggles deliberately not provided

Every syntactic construct is always on.

| Considered        | Status          | Rationale                             |
| ----------------- | --------------- | ------------------------------------- |
| `brace_expansion` | always on       | write `\{a,b\}` for one literal brace |
| `globstar`        | always on       | no real-world need                    |
| `unicode_case`    | not implemented | callers normalize first (§13)         |

> **Rationale.** Each toggle would add parser state for a case that escapes already express.

---

## 4. Lexical Rules

### 4.1 Meta bytes and scopes

The meta bytes are `* ? [ ] { } , \`, plus `!` at the very start of the pattern. Every other byte, and every non-ASCII byte, is literal. The meta bytes fall in three groups:

| Group        | Bytes     | Rule                                                                                                      | Literal form           |
| ------------ | --------- | --------------------------------------------------------------------------------------------------------- | ---------------------- |
| Always meta  | `* ?`     | always a wildcard                                                                                         | `\*`, `\?`             |
| Context meta | `[ ] { }` | an opener starts a scope that MUST be closed; a closer is meta only inside its open scope; `,` separates branches only inside a brace | `\[`, `\]`, `\{`, `\}`, `\,` |
| Prefix       | `!`       | negates only as a leading run (§11); anywhere else it is literal                                          | `\!`                   |

- `[` always opens a class and MUST be closed, else `UnterminatedClass` (§6).
- `{` always opens a brace and MUST be closed, else `UnterminatedBrace` (§6).
- `]` and `}` with no open scope are literal: `a]b` matches `a]b`; `{a,}}` is the brace `{a,}` then a literal `}`, matching `a}` and `}`.
- `|`, `@`, `+`, `(` and `)` have no role at all: `file(1).txt` and `@(a|b)` are literal (§1.2).

All closers share one rule, meta inside their opener's scope and literal outside, and the MUST-close contract belongs to the openers.

### 4.2 The escape `\`

`\` is always the escape character inside a pattern. Its meaning depends on the byte `X` that follows it:

| Next byte          | Result                                                                      |
| ------------------ | --------------------------------------------------------------------------- |
| `/`                | parse error `EscapedSeparator`                                              |
| end of the pattern | parse error `TrailingBackslash`                                             |
| any other `X`      | the literal byte `X`, whether `X` is a meta byte (`\*`) or not (`\a` ≡ `a`) |

The rule is the same inside a class (§8.3) and inside a brace.

> **Rationale.** No file name on any platform contains a `/`, so an escaped separator could never match; reporting it is more useful than compiling a pattern that matches nothing.

`\\` yields a literal `\`. A literal never matches a separator byte (§7.1), so on Windows, where `\` ∈ `Seps`, an escaped backslash never matches: a platform-dependent silent miss (§14.4).

### 4.3 The leading `!`

A run of `!` bytes at position 0 is read before anything else:

1. Count the run.
2. An odd count produces one negation (§11); an even count produces none.
3. The pattern body starts at the first non-`!` byte; every later `!` is a literal byte.

The run belongs to the pattern as a whole. It is read before braces are considered (§10.1), so `{!a,b}` has no negation, and no option disables it (§11.4). A body that is empty after the run (`!`, `!!`) is the `Empty` error (§6): negation applies to a pattern, and there is none.

### 4.4 Scanner independence

The parser is one left-to-right pass. Each opener hands control to a sub-scanner that consumes input by its own rules until it finds its own closer, unaware of the outer context. The first opener owns the input, so an inner closer can be invisible to the outer scanner.

| Pattern  | Trace                                                                          | Result              |
| -------- | ------------------------------------------------------------------------------ | ------------------- |
| `[{]}`   | class holds `{`, closes at `]`; trailing `}` is literal                        | matches `{}`        |
| `{[}]`   | brace opens; the class consumes `}` as a member and closes at `]`; no `}` left | `UnterminatedBrace` |
| `[}{]`   | class holds `}` and `{`                                                        | matches `}` or `{`  |
| `{a,[]}` | brace opens; the class takes `]` as its first member; no second `]` arrives    | `UnterminatedClass` |
| `[(])`   | class holds `(`; trailing `)` is literal                                       | matches `()`        |

> **Rationale.** This is the model of bash brace expansion, POSIX bracket expressions and regex. A pattern that is visually balanced but structurally crossed, such as `{[}]`, is almost always a typo; an error is more useful than bash's silent fallback to literal text.

### 4.5 Length

A pattern longer than 65,536 bytes is the `TooLong` error (§6), measured in UTF-8 bytes.

---

## 5. Grammar and Normalization

### 5.1 Grammar

```bnf
Pattern      ::= [Negation] Sequence
                                   ; the Sequence is not empty (§6, `Empty`)
Negation     ::= "!"+              ; leading run; its parity decides (§4.3, §11)
Sequence     ::= Atom*
Atom         ::= Literal | Separator | AnyChar | Star | Globstar
              |  CharClass | Brace | Escaped

Literal      ::= a byte with no meta role at this position
Separator    ::= "/"
AnyChar      ::= "?"
Star         ::= "*"               ; when not part of a Globstar; a run of
                                   ; stars is one Star (§5.2)
Globstar     ::= "**"              ; exactly two, and only where they own a
                                   ; path segment (§9.1)

CharClass    ::= "[" ("^" | "!")? CharClassItem+ "]"
CharClassItem::= Literal | Range | Escaped
Range        ::= RangeEnd "-" RangeEnd
                                   ; a first-position `]` can start one (§8.5);
                                   ; a `-` right after a Range is a member
RangeEnd     ::= Literal | Escaped ; `[\a-c]` ≡ `[a-c]`

Brace        ::= "{" Sequence ("," Sequence)+ "}"
                                   ; a `{...}` with no comma is literal (§10.4)
                                   ; branches may be empty (§10.3)
Escaped      ::= "\" AnyByte       ; \X yields the literal byte X (§4.2)
```

### 5.2 Normalization

The parser reads these spellings as the forms on the right. They change nothing about the language; they say which construct a spelling is.

| Spelling                       | Reads as         | Rule                                                                 |
| ------------------------------ | ---------------- | -------------------------------------------------------------------- |
| a run of stars `***`           | one `*`          | a star run inside a segment is one star                              |
| `**` that does not own a segment (`a**b`, `a/**b`, `a**/b`) | `*` (`a*b`, `a/*b`, `a*/b`) | §9.1: a globstar is preceded by `/` or the start and followed by `/` or the end |
| `**/**/` (consecutive)         | one `**/`        | §9.6                                                                 |
| `{a}` (a brace with no comma)  | the literal `{a}` | §10.4; the `{` and `}` become plain bytes, so a `**` beside them degrades |
| `\X` with an ordinary `X`      | `X`              | §4.2                                                                 |
| `[a-]`, `[-a]`                 | `a` or `-`       | §8.3: a `-` at a class edge is a member                              |
| `a//b`                         | kept as written  | each `/` is its own strict separator (§7.4); only the boundaries of a `**` are lenient (§9.5) |

Next to a brace, whether a `**` owns a segment is judged on each expansion (§10.1), so the second row applies per expansion.

---

## 6. Errors and Limits

### 6.1 Compile-time errors

Compilation MUST reject the following inputs (`TooComplex`: MAY, §6.2). It has no other failure modes: a pattern either compiles into a matcher or raises one of these.

| Error                    | Example                    | Reason                                                             |
| ------------------------ | -------------------------- | ------------------------------------------------------------------ |
| `Empty`                  | `""`, `!`, `!!`            | no pattern to match or to negate (§4.3)                            |
| `TooLong`                | more than 65,536 bytes     | resource guard (§4.5)                                              |
| `UnterminatedClass`      | `[abc`, `[]`, `[/]`        | an opener needs its closer (§4.1); a `/` ends the segment (§8.2); a first `]` is a member (§8.5) |
| `UnterminatedBrace`      | `{a,b`                     | an opener needs its closer (§4.1)                                  |
| `TrailingBackslash`      | `foo\`                     | nothing to escape (§4.2)                                           |
| `EscapedSeparator`       | `a\/b`                     | no file name holds a `/` (§4.2)                                    |
| `InvalidRange`           | `[z-a]`                    | the end is below the start (§8.1)                                  |
| `BraceNestingTooDeep`    | 33 nested `{`              | resource guard (§6.2)                                              |
| `BraceExpansionTooLarge` | `{**,a}{/,b}` × 5          | resource guard for the expansion around `**` (§6.2)                |
| `TooComplex`             | `{a,b,/}` × 100            | resource guard for compile work (§6.2)                             |
| `EmptyPatternSet`        | `globstar([])`             | a union needs at least one pattern (§16.1)                         |
| `NegatedInUnion`         | `Glob::union(["a", "!b"])` | Rust only: a union rejects `!` members (§16.1)                     |

A stray `]`, `}`, `(` or `)` is not an error; it is a literal byte (§4.1). The lenient spellings of §5.2 are not errors either.

### 6.2 Limits

Three bounds keep compilation linear in practice. Each is a resource guard, not a rule of the language: a pattern inside the bounds means what §7–§13 say, and one outside them is refused.

- **Nesting.** An implementation MUST accept braces nested up to 32 levels deep and MUST reject deeper nesting with `BraceNestingTooDeep`. The depth counts every `{` as it opens, including one that later turns out to be literal (§10.4). The limit also bounds the local expansion around `**` (§10.1), which can nest braces the pattern wrote side by side: `**` followed by `{,/a}` 33 times fails the same way.
- **Expansion around `**`.** Branch counts and Cartesian breadth are not bounded, because nothing is pre-expanded (§10.5). The one bounded thing is the local expansion that judges a `**` beside a brace (§10.1). Every copy it makes is counted: one per token, a literal or class by its length, and one per brace branch. When the count would pass 4096, compilation MUST fail with `BraceExpansionTooLarge`. The count runs over the whole pattern, and over all patterns of one union together (§16.1). Two shapes reach the cap: a chain of braces that each put a `**` beside the next, since each link copies what the chain built so far (`{**,a}{/,b}` repeated five times); and a `**`-edged brace beside a long list of mixed branches, which is copied once. Branches that all start with `/` are not copied (`{**,dist}{/a,/b,…}`, `**{/a,/b,…}`), and neither is a brace that only shares a `/` with the next (`{**,a}/` repeated any number of times).
- **Compile work.** Braces whose branches cross a `/` can make the segments of a pattern multiply: `{a,b,/}` repeated k times has about k² distinct segments. An implementation MAY fail such a pattern with `TooComplex`, and MUST NOT fail one whose compile work stays linear in its length. The bound and how work is counted are implementation-defined; `max` reports the bound that was passed. (Informative: the reference implementations allow 64 units of work per unit of pattern size, and at least 2^20 units; a unit is a node, an edge, a copied token, or a byte of the tables a segment builds. Ordinary patterns and big unions need one to three per unit, so only the multiplying shapes reach the bound, at a few hundred bytes for the shape above.)

### 6.3 Reference error types

`at` is a byte offset into the UTF-8 pattern (for `InvalidRange`, where the range starts). An error that one member of a pattern list raises names the member by its position in the list, counting every member given: Rust wraps it in `InMember`, JS adds `index` and keeps the member's `kind`. A `TooComplex` is raised by the list as a whole, which compiles as one pattern (§16.1), and names no member.

```rust
// Rust
pub enum GlobError {
    Empty,
    TooLong { len: usize, max: usize },
    UnterminatedClass { at: usize },
    UnterminatedBrace { at: usize },
    TrailingBackslash,
    EscapedSeparator { at: usize },
    BraceNestingTooDeep { max: usize },
    BraceExpansionTooLarge { max: usize },
    TooComplex { max: usize },
    InvalidRange { at: usize, low: u8, high: u8 },
    EmptyPatternSet,
    NegatedInUnion { index: usize, pattern: String }, // Rust only (§16.1)
    InMember { index: usize, error: Box<GlobError> },   // a union member's error
}
```

```ts
// JS: the same names on `.kind`, the same fields as properties
class GlobError extends Error {
  readonly kind:
    | "Empty" | "TooLong" | "UnterminatedClass" | "UnterminatedBrace"
    | "TrailingBackslash" | "EscapedSeparator" | "BraceNestingTooDeep"
    | "BraceExpansionTooLarge" | "TooComplex" | "InvalidRange" | "EmptyPatternSet";
  readonly at?: number; readonly len?: number; readonly max?: number;
  readonly low?: number; readonly high?: number;
  readonly index?: number; // a list member's error: the member's position
}
```

---

## 7. Literals, `?`, `*` and `/`

### 7.1 Literal

A literal byte matches exactly that byte, once. A literal that is a separator byte on the platform (`\` on Windows, §2.2) matches nothing: a pattern `/` is the only way to consume a separator.

### 7.2 `?`

Matches exactly one byte that is not a separator (§2.2). It does not match the empty string, and one byte is one byte, not one character (§14.3).

### 7.3 `*`

Matches zero or more non-separator bytes.

- It matches the empty string: `foo*bar` matches `foobar`.
- It does not cross a separator: `src/*` does not match `src/a/b`.

A run of stars inside a segment is one star (§5.2).

### 7.4 `/`

A pattern `/` consumes exactly one separator byte of the path: `a/b` does not match `a//b`, and `a//b` requires two separators. The two places where a run of separators is accepted both belong to the globstar (§9.5).

---

## 8. Character Classes

A class is one construct that consumes one byte, chosen from a set.

### 8.1 Forms

| Form       | Kind    | Matches                                |
| ---------- | ------- | -------------------------------------- |
| `[abc]`    | set     | `a`, `b` or `c`                        |
| `[a-z]`    | range   | any byte in `a..=z`                    |
| `[abc0-9]` | mixed   | a set member or a range member         |
| `[!abc]`   | negated | any byte that is none of `a`, `b`, `c` |
| `[^abc]`   | negated | the same as `[!abc]`                   |

A range whose end is below its start (`[z-a]`) is the `InvalidRange` error (§6). The check is byte-level and case-sensitive whatever the options (§13).

### 8.2 A class is segment-local

A class closes inside the segment that contains it. A `/` in the class body, written directly (`[/]`, `[a/b]`, `[^/]`) or escaped (`[\/]`), ends the segment before a closer was found, and the parser MUST report `UnterminatedClass`.

At match time a class never matches a member of `Seps`, whatever its polarity. `/` is always in `Seps`, so this rule only adds something on platforms where `\` is a separator (Windows, §14.4): there `[^abc]` rejects `\`, and the positive class `[\\]` never matches. On Unix `\` is an ordinary byte and `[\\]` matches it.

> **Rationale.** Compiling `foo[/]bar` to a class that can never match would hide the mistake; an error reports it at once.

### 8.3 Special bytes inside a class

| Byte                                   | Role            | When                                                     |
| -------------------------------------- | --------------- | -------------------------------------------------------- |
| `]`                                    | closer          | not in first position (first position: §8.5)             |
| `-`                                    | range separator | between two members, a first-position `]` included; a member next to `[` or the closer, or right after a range |
| `!`, `^`                               | negation marker | immediately after `[`                                    |
| `\`                                    | escape prefix   | always; the next byte becomes a literal member (§4.2)    |
| `/`                                    | illegal         | any position: `UnterminatedClass` (§8.2)                 |
| `*`, `?`, `{`, `}`, `(`, `)`, `@`, `+` | literal         | meta bytes from outside a class are plain members inside |
| any other byte                         | literal         |                                                          |

Escapes inside a class follow §4.2: `\X` is the member `X`, whatever `X` is. Thus `[\?]` holds `?`, `[\-]` holds `-` (no range), `[\!]` is a positive class holding `!`, and `[\]]` holds `]` because the escape consumes the first `]` and the second closes.

| Pattern            | Result                  | Why                                                  |
| ------------------ | ----------------------- | ---------------------------------------------------- |
| `[?*!(]`           | class `{?, *, !, (}`    | meta bytes are plain members inside                  |
| `[a-z]`            | range `a..=z`           | `-` between two members                              |
| `[a\-z]`           | class `{a, -, z}`       | the escaped `-` is a member, not a range             |
| `[\a-c]`, `[a-\c]` | range `a..=c`           | an escaped byte can end a range                      |
| `[a-c-e]`          | class `{a..=c, -, e}`   | a `-` right after a range is a member                |
| `[]-a]`            | range `]..=a`           | a first-position `]` can start a range (§8.5)        |
| `[-abc]`, `[abc-]` | class with `-`          | `-` next to a bracket is literal                     |
| `[!abc]`           | negated `{a, b, c}`     | `!` in first position                                |
| `[\!abc]`          | positive `{!, a, b, c}` | the escape overrides the negation                    |
| `[]]`              | class `{]}`             | first-`]` rule (§8.5)                                |
| `[!]]`             | negated `{]}`           | first-`]` after the negation marker                  |
| `[\]]`             | class `{]}`             | escaped form                                         |
| `[\]`              | `UnterminatedClass`     | the escape consumes the only `]`                     |
| `[a\`              | `TrailingBackslash`     | the pattern ends inside the escape                   |
| `[]`               | `UnterminatedClass`     | first `]` is a member; no closer follows             |
| `[/]`, `[\/]`      | `UnterminatedClass`     | separator in a class (§8.2)                          |
| `[\\]`             | class `{\}`             | matches `\` on Unix; never matches on Windows (§8.2) |

### 8.4 Negated classes and the dot option

A negated class is a wildcard for the purpose of dot protection (§12): under `dot = false`, `[!a]` at a segment start does not match `.`; under `dot = true` it does. A positive class with `.` as a member (`[.]`, `[.a]`) matches `.` under either setting, because it names the dot.

### 8.5 The POSIX first-`]` rule

A `]` immediately after `[`, `[!` or `[^` is a member, not the closer, as in bash, fnmatch, picomatch, globset and fast-glob. Consequences:

- `[]]` matches `]`; `[!]]` matches any byte except `]`; `[]-]` matches `]` or `-`.
- The first `]` is a member like any other, so it can start a range: `[]-a]` is the range `]..=a` (with `^`, `_` and `` ` `` in it), as in POSIX, bash and picomatch.
- `[]`, `[!]` and `[^]` are `UnterminatedClass`: the `]` was a member, and no closer follows.
- `[` alone is `UnterminatedClass`.
- `[a-]` is the class `{a, -}`: a trailing `-` is a member.

### 8.6 A class takes one byte

Like every construct (§14.3), a class matches one byte. A non-ASCII character in it stands for its UTF-8 bytes, each a member of its own, and a range with a non-ASCII end ranges over bytes: `[é]` is the class {`0xC3`, `0xA9`} and never matches `é`, which is two bytes. To match one of several characters, use a brace: `{é,è}`.

### 8.7 POSIX character classes are not recognized

`[[:alpha:]]` parses as the class `[[:alpha:]` (members `[`, `:`, `a`, `l`, `p`, `h`) followed by a literal `]`. It matches `a]` and never a letter on its own (§1.2).

---

## 9. The Globstar `**`

`**` is the only construct allowed to cross a separator; that is its sole difference from `*`.

### 9.1 A globstar owns a segment

`**` is a globstar if and only if it is preceded by `/` or the start of the pattern, and followed by `/` or the end of the pattern. Otherwise it is two `*`, which collapse to one (§5.2).

| Pattern       | `**` is a globstar? | Reads as |
| ------------- | ------------------- | -------- |
| `src/**/*.ts` | yes                 |          |
| `**/*.ts`     | yes                 |          |
| `**`          | yes                 |          |
| `src/**`      | yes                 |          |
| `a**b`        | no                  | `a*b`    |
| `a/**b`       | no                  | `a/*b`   |
| `a**/b`       | no                  | `a*/b`   |

Next to a brace the test uses the expanded form (§10.1), one expansion at a time. The neighbor of a `**` beside a brace is the first or last token of each branch, or whatever lies beyond the brace for an empty branch; the neighbor of a `**` at a branch edge is whatever surrounds the brace. The same `**` can be a globstar in some expansions and a star in others.

| Pattern      | Expansions       | Note                                                        |
| ------------ | ---------------- | ----------------------------------------------------------- |
| `a{**,x}b`   | `a*b` ∪ `axb`    | degrades                                                    |
| `{**,x}/b`   | `**/b` ∪ `x/b`   | matches `b`                                                 |
| `{a/**,x}c`  | `a/*c` ∪ `xc`    | degrades                                                    |
| `a/{**/x,y}` | `a/**/x` ∪ `a/y` | a globstar with the lenient `/**/` boundary; matches `a//x` |
| `**{/a,b}`   | `**/a` ∪ `*b`    |                                                             |
| `{a/,b}**`   | `a/**` ∪ `b*`    |                                                             |
| `{a,}**`     | `a*` ∪ `**`      | the empty branch leaves the `**` alone                      |

### 9.2 Language

A globstar matches any byte sequence, separators included, the empty sequence included. With Σ the set of all bytes:

```
L(**)      = Σ*
L(a/**)    = { "a/" + w : w ∈ Σ* }                        ; includes "a/"
L(**/b)    = { w + "b" : w ∈ Σ*, w empty or ending in / }
L(a/**/b)  = { "a/" + w + "b" : w empty or ending in / }  ; a/b, a/x/b, a/x/y/b, …
```

Under `dot = false` every segment start a globstar exposes is still dot-protected (§12): `**` does not match `a/.b` or `.a/b`.

### 9.3 `a/**` against the forms of `a`

| Path    | `is_match` | Reason                     |
| ------- | ---------- | -------------------------- |
| `a`     | false      | the prefix `a/` is missing |
| `a/`    | true       | `a/` plus the empty suffix |
| `a/b`   | true       |                            |
| `a/b/c` | true       |                            |

This agrees with globset, minimatch and the fast-glob crate. It deliberately differs from picomatch (unless `strictSlashes: true`) and doublestar, which make `a/**` match `a`. To select a directory and everything below it, write `{a,a/**}`. §15.3 gives the `match_dir` answers for the same forms.

### 9.4 Leading `**/` ("at any depth")

A pattern that begins with `**/X` means "X at any depth" and accepts every kind of root:

- relative paths: `foo`, `a/foo`, `a/b/foo`;
- Unix absolute paths: `/foo`, `/a/foo`;
- UNC paths: `//server/foo`, `//server/share/foo`;
- drive-letter paths: `C:/a/foo`, and `C:\a\foo` where `\` is a separator (Windows). `C:` is an ordinary segment, so no special case is needed.

Formally `**/X ≡ (SEP | seg/)* X`, where `SEP` is one separator byte and `seg` a non-empty run of non-separator bytes: an optional run of root separators, then zero or more whole segments. A pattern that begins with `/**/X` requires the absolute form.

| Pattern | `/` | `/b` | `a/b` | `b` |
| ------- | --- | ---- | ----- | --- |
| `**`    | ✅  | ✅   | ✅    | ✅  |
| `**/`   | ✅  | ❌   | ❌    | ❌  |
| `**/b`  | ❌  | ✅   | ✅    | ✅  |
| `/**/b` | ❌  | ✅   | ❌    | ❌  |

`**/*.rs` therefore means "every `.rs` file", relative or absolute. Write `/**/*.rs` to restrict to absolute paths.

With braces this follows from §10.1. In `{**/a,README}` only the `**/a` expansion begins with `**/`, so the pattern matches `/a` but not `/README`: a literal branch stays strict. A `**/` in the middle of a pattern (`a/{**/x,y}`) is not leading; it keeps the lenient boundary of §9.5 and nothing more.

### 9.5 Lenient separators beside a globstar

A pattern `/` consumes one separator byte (§7.4). Separator runs are accepted in exactly two places, both at a globstar:

- the `/` on either side of a `**`: `a/**/b` matches `a//b`, as in picomatch and globset. This holds across braces too: `{a,/**}/{**/x,b}` matches `a//x`;
- a pattern-leading `**/`, which also accepts zero or more leading separators (§9.4): `**/foo` matches `foo`, `/foo` and `//server/share/foo`.

### 9.6 Consecutive and independent globstars

`**/**/` is one `**/` (§5.2); the language is unchanged. Two globstars in one pattern are independent, in different branches of a brace too: in `{src/**,lib/**}` neither is merged with or aliased to the other.

---

## 10. Braces

### 10.1 The expansion equation

A brace is pure union. For any fragments `pre`, `post` and branches `A`, `B`, …:

```
L( pre{A,B,…}post ) = L( preApost ) ∪ L( preBpost ) ∪ …
```

Each expansion is read as a standalone pattern under every rule of this document. In particular, `**` segment ownership (§9.1) is judged on the expanded form. A brace neither grants a `**` powers it lacks outside (`a{**,x}b` ≡ `a*b` ∪ `axb`, because the expanded neighbors are `a` and `b`) nor takes away powers it has (`{**,x}/b` ≡ `**/b` ∪ `x/b`, which matches `b`). The same holds for the lenient `/**/` boundary (§9.5), the leading `**/` (§9.4) and dot protection (§12): `x{/,y}{**,a}/z` contains `x/**/z` and matches `x//z`.

Two exceptions keep the equation from over-applying:

1. The leading `!` is read before expansion (§4.3). `{!a,b}` expands to the body `!a`, whose `!` is a literal byte.
2. An empty expansion means ε, not an error. The empty branch of `{a,}` contributes the empty string; the `Empty` error (§6) applies to the user's pattern only.

The equation works on tokens, not bytes. A branch is spliced in as a unit and tokens never fuse across the splice: `*{,a}*` is `L(* ε *)` ∪ `L(* a *)`, and the two stars of the empty expansion stay two stars (which collapse to one, §5.2). They do not become a `**` the author never wrote.

> **Implementation note.** The equation defines meaning only; §10.5 still forbids pre-expanding braces, and an implementation need only be equivalent. The reference implementations decide a `**` beside plain tokens while parsing. A `**` whose neighbor is a brace edge that can be a separator, or empty, is handled by distributing that brace over it (`**{/a,b}` → `{**/a,**b}`, `{a/,b}**` → `{a/**,b**}`); two braces meeting at a `**` edge (`{**,a}{/,b}`) merge into one. Only the branches the `**` can affect are taken apart, and branches that put a `/` toward the `**` share that one `/` (`**{/a,/b}` → `**/{a,b}`). This local expansion is what the nesting and expansion limits of §6.2 bound.

### 10.2 Forms

| Form         | Matches                                   |
| ------------ | ----------------------------------------- |
| `{a,b,c}`    | `a`, `b` or `c`                           |
| `{a*b,c?}`   | a branch may hold wildcards               |
| `{a,{b,c}}`  | nested braces: `a`, `b` or `c`            |
| `{a,b}{c,d}` | Cartesian product: `ac`, `ad`, `bc`, `bd` |

### 10.3 Empty branches

`{a,}` is legal and means `a` or the empty string.

### 10.4 Single-branch braces

`{a}` (no comma) is not a brace. It is the literal three bytes `{a}`, as in bash, picomatch and the npm fast-glob. globset, doublestar and the fast-glob crate read it as a one-branch group instead, so there `{a}` matches `a`.

The now-literal `{` or `}` is a plain byte, not a segment boundary, so a `**` next to it degrades to `*` (§9.1): `{**}` ≡ `{*}` matches `{a}` but not `{a/b}`, and `{a/**}` ≡ `{a/*}`. A `**` with `/` on both sides stays a globstar: `{a/**/b}` matches `{a/x/y/b}`. This is a deliberate divergence from picomatch and the npm fast-glob, which keep the globstar; bash degrades it the same way (Appendix A).

A separator stays structural; it never becomes literal content. `x{a/b}y` is the two-segment pattern `x{a` / `b}y`, exactly like `x\{a/b\}y`.

> **Rationale.** Brace expansion needs alternatives: `bash -c 'echo {a}'` prints `{a}`, while `echo {a,b}` prints `a b`. To match a file name with braces, write them bare (no comma, no brace) or escape them (`\{a\}`). Classes differ: `[a]` is still a class, because bracket expressions never had a minimum-member rule.

### 10.5 No pre-expansion

An implementation MUST NOT expand braces into N separate patterns at parse time; braces are built into the matcher. Memory and match cost therefore do not blow up on `{a,b}{c,d}{e,f}…`. Braces whose branches cross a `/` are the exception: they can make the segments of a pattern multiply, and compilation may then fail with `TooComplex` (§6.2).

### 10.6 Bounds

An unclosed brace (`{a,b`) is `UnterminatedBrace`. Nesting is bounded at 32 levels and the local expansion around `**` at 4096 copies, shared across the members of a union; both are stated in §6.2.

### 10.7 Braces with other constructs

- A branch may contain `**`: `{src/**/*.ts,lib/**/*.js}`. The two globstars are independent (§9.6), and each is judged on its expansion (§9.1).
- `{[ab],c}` is legal. Brace and class scopes are independent (§4.4).
- Dot protection is judged per expansion (§12).

---

## 11. Negation

### 11.1 Parity

A run of `!` at the start of a pattern (§4.3) negates the whole pattern: `!P` matches exactly the paths `P` does not. An odd run negates, an even run cancels (`!!foo` ≡ `foo`, `!!!foo` ≡ `!foo`).

```
L(!P) = Σ* \ L(P)
```

### 11.2 Whole-pattern only

There is no segment-level negation, and the run is read before braces are considered (§10.1). A `!` outside the leading run is a literal byte: `!(foo)` is one negation over the literal body `(foo)`, matching every path except `(foo)`; `{!a,b}` has no negation.

### 11.3 A literal leading `!`

To match a name that starts with `!`, escape it: `\!foo` matches `!foo`. `[\!]foo` works too. `[!]foo` does not: by §8.5 its `]` is a member and the class is unterminated.

### 11.4 Options

The leading `!` is part of the pattern syntax, not a matching option; no option disables it. §15.4 and §16.1 say what a negated pattern means to `match_dir` and to a union.

---

## 12. Dot Protection

Under `dot = false` (§3.1), a path position `i` is **dot-protected** when `path[i] == '.'` and `i` is a segment start (§2.3): the `.` that starts a segment. A segment that starts there matches only a pattern segment whose first element takes the `.` by name:

- a literal `.`, or a positive class that lists `.` (a range counts: `[,-.]`), takes it as usual;
- a `*` there matches nothing, not even the empty string: `*.txt` does not match `.txt`, and `*.*` does not match `.a`;
- `?` and a negated class do not match the `.`;
- braces are judged per expansion (§10.1): `{*,.}a` matches `.a` through its expansion `.a`, and `*{.a,b}` does not;
- `**` itself is unaffected, but every segment start it exposes is subject to the rule: `**` does not match `a/.b` or `.a/b` (§9.2).

Under `dot = true` no position is protected, and `*`, `?`, negated classes and `**` consume a leading `.` like any byte (the names `.` and `..` included, §3.1).

---

## 13. Case Folding

Under `case_insensitive` (§3.2), ASCII letters compare case-folded wherever a pattern byte meets a path byte: `*.ts` matches `MAIN.TS`; `[A-Z]` matches `a`.

Case folding is a matching behavior, not a syntax rule. The parser MUST NOT consult the flag, so every validity check stays byte-level and case-sensitive: `[a-A]` is `InvalidRange` even under `case_insensitive = true`, and error messages always describe the user's literal input.

Folding covers ASCII only (§14.3). Non-ASCII bytes compare verbatim: `café.txt` matches `CAFé.txt` but not `CAFÉ.txt`. Callers who need Unicode folding MUST normalize both pattern and path first (for example NFKC plus lowercase).

Static prefixes are reported in the pattern's case (§17); WALKER_SPEC.md §5 says how the walker seeds under this option.

---

## 14. Matching a Path

### 14.1 Whole-path matching

`is_match(P, path) := path ∈ L(P)` (§2.4). A match covers the whole path: every byte of the path is consumed by `P`. There is no substring matching, and no implicit anchoring beyond that.

### 14.2 Separators on the path side

The matcher recognizes the separator set `Seps` of §2.2 wherever it asks whether a byte is a separator. Each pattern `/` consumes exactly one separator byte (§7.4); runs are accepted only at a globstar (§9.5).

What the matcher does not do:

1. Rewrite `\` to `/`: a separator byte is recognized, never substituted.
2. Collapse `//` in a pattern: each `/` is its own strict separator.
3. Collapse `//` in a path: only the two lenient places of §9.5 absorb runs.
4. Resolve `.` or `..`: they are matched as bytes.
5. Resolve symlinks: a walker-layer concern.

### 14.3 Bytes, not characters

Matching is byte-wise (§2.1). `?` consumes one byte, not one code point: to match one three-byte CJK character, write `???`. A class also takes one byte (§8.6), and case folding sees ASCII bytes only (§13). This is an acknowledged non-goal.

### 14.4 Platform differences

The same pattern and path MAY match on one platform and not on another, because `Seps` differs (§2.2): on Windows a path may use `\`, a literal `\` in a pattern (`\\`) matches nothing, and a class never matches `\` (§7.1, §8.2). This is the cost of accepting native paths. Callers who need portable results SHOULD normalize paths to `/` first.

---

## 15. Directory Matching `match_dir`

### 15.1 Definition

For a pattern `P` and a directory path `d`, `match_dir(P, d)` returns one of:

```rust
enum DirMatch {
    Match,               // d ∈ L(P); nothing below d is in L(P)
    Pruned,              // d ∉ L(P); nothing below d is in L(P)
    Descend,             // d ∉ L(P); something below d may be
    DescendAndMatch,     // d ∈ L(P); something below d may be too
    DescendAll,          // d ∉ L(P); every path below d is in L(P)
    DescendAllAndMatch,  // d ∈ L(P); every path below d is in L(P)
}
```

The result answers three questions: does `d` match (`matches`), may something below it match (`may_match_below`), and does everything below it match (`matches_all_below`, §15.5). The third implies the second.

The names read from the pattern's side; what a walker does depends on the pattern's role. A walker that selects files descends into `d` while `may_match_below` holds. A walker that excludes files skips `d` whole once `matches_all_below` holds, and stops testing under `d` once `may_match_below` is false.

### 15.2 Semantics

```
below(d)         := Σ⁺                                if d = ""   (the walk root)
                    { d ++ "/" ++ w : w ∈ Σ* }        otherwise

matches(P, d)    := d ∈ L(P)
may_below(P, d)  := below(d) ∩ L(P) ≠ ∅
all_below(P, d)  := §15.5

match_dir(P, d) :=
  match (matches, may_below, all_below):
    (true,  _,     true ) → DescendAllAndMatch
    (false, _,     true ) → DescendAll
    (true,  true,  false) → DescendAndMatch
    (true,  false, false) → Match
    (false, true,  false) → Descend
    (false, false, false) → Pruned
```

Both flags about "below" are one-sided (§15.5). A `false` may-below is a guarantee that nothing below matches; `true` MAY be conservative. The reference implementations report may-below at the root for every pattern, since nothing has been consumed there.

### 15.3 The query path

`d` is taken as given and never canonicalized. `match_dir("src", "src")` is `Match`. A trailing slash asks about that exact byte string: `match_dir("a/**", "a/")` is `DescendAndMatch` under `dot = false` (`a/` ∈ L and `a//x` ∈ L, but `a//.x` ∉ L; under `dot = true` every path below matches, so it is `DescendAllAndMatch`), while `match_dir("a/b", "a/")` is `Pruned` (nothing in `{a/b}` starts with `a//`).

Walkers get directory paths from `readdir` without a trailing separator, so the slashless form is the canonical query: ask about `a`, not `a/`, and about a drive as `C:`, not `C:/`. The filesystem root has no slashless form, and `/` asks about the paths below `//`, so `match_dir("/p/**", "/")` is `Pruned`; ask about `""` (the walk root) for it instead, whose answers hold for every path below (may-below is always reported there, and all-below only when every path matches).

The forms of `a` against `a/**` (§9.3), under `dot = false`:

| Directory | `match_dir("a/**", d)` | Reason                                                   |
| --------- | ---------------------- | -------------------------------------------------------- |
| `a`       | Descend                | `a` does not match; `a/x` may                            |
| `a/`      | DescendAndMatch        | `a/` matches; `a//x` matches too (lenient boundary, §9.5) |
| `a/b`     | DescendAndMatch        |                                                          |

Under `dot = true` every path below `a` matches, so the three become `DescendAll`, `DescendAllAndMatch` and `DescendAllAndMatch` (§15.5).

### 15.4 Negated patterns

A negated pattern returns `DescendAndMatch` when `d` itself matches and `Descend` otherwise, and never a covering result (§15.5): `matches` stays exact (§15.2) while the below flags say only "something may". In a union (§16.2) `matches` is the union's own answer for `d`, and a negated member contributes may-below.

> **Rationale.** Inverting the body's result would tell a walker almost nothing: `!P` normally means "everything except P", and the walker must visit the whole tree to find it.

### 15.5 All-below: `DescendAll` and `DescendAllAndMatch`

#### Definition

```
all_below(P, d) := ∀ s ∈ R.  (d = "" ? s : d ++ "/" ++ s) ∈ L(P)
R               := seg ( "/" seg )*        ; one or more segments
seg             := one or more non-separator bytes
```

Whether `d` itself matches plays no part.

> **Note.** A pattern that excludes files says nothing about a directory by matching it: `**/*.log` matches a directory named `foo.log` but not `foo.log/a.js` inside. A consumer that wants to skip a whole directory, such as a watcher applying an exclude list, needs to know that nothing below the directory escapes the pattern. That is what this flag tells it.

#### Guarantee

The flag is one-sided, the mirror image of may-below:

| Flag                            | `true` means                          | `false` means                      |
| ------------------------------- | ------------------------------------- | ---------------------------------- |
| may-below (`may_match_below`)   | something below may match             | nothing below matches: a guarantee |
| all-below (`matches_all_below`) | everything below matches: a guarantee | not everything, or not provable    |

An implementation MUST NOT set all-below unless `all_below(P, d)` holds. A consumer that skips a directory only on `matches_all_below` therefore never hides a path the pattern does not match.

#### Reference rule

The reference implementations decide the flag segment by segment. Under `dot = false` it is never set: `**` does not match `d/.cache`, so no subtree is covered. Under `dot = true` they continue from the state `d` leaves the matcher in and step it over a _wildcard-only segment_: a name of any length whose bytes only `?`, `*` and `**` consume (literals and classes, negated classes included, take none). A globstar absorbs the segment; an element inside a segment takes it only if its wildcards alone take every non-empty name (`*` and `?*` do; `?` and `*.js` do not); a literal never does. The state must accept after every step, and the flag is set once a step leaves the state unchanged. A real segment is taken wherever a wildcard-only one is (no wildcard is dot-protected under `dot = true`), which gives the guarantee.

Consequences:

- A pure literal pattern never sets it, and neither does a negated pattern (§15.4).
- A union sets it when its members cover the subtree together, not only when one member does (§16.2). Members that differ in one segment are merged first (`["**/?", "**/??*"]` becomes one element, `**/?{,?*}`, and is proven at `a`); a brace the user wrote is not regrouped, so `{**/?,**/??*}` keeps its two branches and gets `DescendAndMatch` at `a`, though it matches the same paths.

#### Examples

All rows use `dot = true` unless marked.

| Pattern                      | Dir                   | Result             | A path below that escapes |
| ---------------------------- | --------------------- | ------------------ | ------------------------- |
| `/p/dist/**`                 | `/p/dist`             | DescendAll         | none                      |
| `**/node_modules/**`         | `/p/node_modules`     | DescendAll         | none                      |
| `**/node_modules/**`         | `/p/node_modules/pkg` | DescendAllAndMatch | none                      |
| `**`                         | `/p/a`                | DescendAllAndMatch | none                      |
| `/p/dist/**/*`               | `/p/dist`             | DescendAll         | none                      |
| `/p/dist/**/{a,*}`           | `/p/dist`             | DescendAll         | none                      |
| `{/p/dist/*,/p/dist/*/**}`   | `/p/dist`             | DescendAll         | none                      |
| `/p/dist/**/*.js`            | `/p/dist`             | Descend            | `/p/dist/a`               |
| `/p/dist/*/**`               | `/p/dist`             | Descend            | `/p/dist/a`               |
| `/p/dist/*`                  | `/p/dist`             | Descend            | `/p/dist/a/b`             |
| `/p/dist/**/?`               | `/p/dist`             | Descend            | `/p/dist/ab`              |
| `**/*.log`                   | `/p/foo.log`          | DescendAndMatch    | `/p/foo.log/a`            |
| `/p/dist/**` (`dot = false`) | `/p/dist`             | Descend            | `/p/dist/.cache`          |

#### Not provable

Two shapes cover a subtree without the reference rule seeing it, and get the safe `Descend`:

- coverage through classes: `/p/dist/**/{a*,[!a]*}` takes every name, as one starting with `a` or one that does not;
- coverage by name length across branches: `?/**/?` with `**/??*` takes every name below `c/x.js`, as a one-byte name or a longer one (and so does a brace like `{**/?,**/??*}`, above).

The rule asks a single element to take every name, and neither shape has one.

The reference implementations also bound the proof. They feed at most 65 wildcard-only segments (64 states' worth and the step that shows the set has settled), and a matcher whose state has not settled by then leaves all-below unset: the union of `**` and `**/` + `*/` × 100 + `x` covers everything below `a`, and is not proven. For a single path with one `**`, they spend at most 4096 element compares on the tail positions a directory may leave; past that they set may-below and leave all-below unset, which are the safe answers of both flags.

### 15.6 Examples

| Pattern             | Dir                              | Result          | Reason                                    |
| ------------------- | -------------------------------- | --------------- | ----------------------------------------- |
| `src/**/*.ts`       | `src`                            | Descend         | a `.ts` may lie below                     |
| `src/**/*.ts`       | `src/components`                 | Descend         |                                           |
| `src/**/*.ts`       | `tests`                          | Pruned          | not under `src`                           |
| `src/**/*.ts`       | `node_modules`                   | Pruned          | not under `src`                           |
| `src/**/*.ts`       | `src/main.ts` (queried as a dir) | DescendAndMatch | matches now; `src/main.ts/x.ts` would too |
| `src/components`    | `src`                            | Descend         |                                           |
| `src/components`    | `src/components`                 | Match           | exact; nothing continues below            |
| `src/components/**` | `src/components`                 | Descend         | `**` needs the `/` after it               |
| `src/components/**` | `src/components/a`               | DescendAndMatch |                                           |
| `{src,lib}/**`      | `src`                            | Descend         |                                           |
| `{src,lib}/**`      | `docs`                           | Pruned          |                                           |
| `**/*.ts`           | `any/dir`                        | Descend         | `**` allows any depth                     |

These are the `dot = false` results. Under `dot = true` the rows whose pattern ends in `/**` and whose directory is at or below that `/**` report the covering results instead: `src/components/**` gives `DescendAll` on `src/components` and `DescendAllAndMatch` on `src/components/a`; `{src,lib}/**` gives `DescendAll` on `src` (§15.5).

> **Implementation note.** Feed `d` to the matcher and classify where it stops: no live state is `Pruned`; an accepting state is `Match` or `DescendAndMatch`; live states without acceptance are `Descend`. For the all-below flag, continue from those states as §15.5 describes.

---

## 16. Unions

### 16.1 Definition

A union is the boolean OR of N patterns. In Rust, `Glob::union` and `Glob::union_with` compile the members into one `Glob`; there is no separate set type. In JS, `globstar(patterns)` returns a predicate and `compileMatcher(patterns)` returns a matcher with `match`, `matchDir` and `staticPrefixes`.

```rust
// Rust
impl Glob {
    pub fn union<I, S>(patterns: I) -> Result<Glob, GlobError>;
    pub fn union_with<I, S>(patterns: I, opts: CompileOptions) -> Result<Glob, GlobError>;
}
```

```ts
// JS
function globstar(patterns: string | Iterable<string>, opts?: GlobstarOptions): (input: string) => boolean;
function compileMatcher(patterns: string | Iterable<string>, opts?: GlobstarOptions): Matcher;
```

```
union.is_match(path) ≡ ∃ g ∈ members.  g.is_match(path)
```

A union answers the boolean questions only (`is_match`, `match_dir`, `static_prefixes`); which member matched is not part of the API. An empty list is `EmptyPatternSet` (§6). A pattern given twice counts once. All members share one expansion budget and, compiled as one pattern, one bound on compile work (§6.2): members that each compile alone can fail together with `TooComplex`. An error one member raises names the member; `TooComplex` names none (§6.3).

Negated members differ by language. Rust rejects them (`NegatedInUnion`, §6). JS accepts them, and each contributes its complement to the OR: `["src/**", "!**/*.test.ts"]` matches `lib/a.ts`, because the second member does. A `!` member never subtracts from the others; include/exclude filtering belongs to the walker (WALKER_SPEC.md §4).

### 16.2 Combined `match_dir`

```
union.match_dir(d) := combine(
  ∃ g. matches(g, d),
  ∃ g. may_below(g, d),
  all_below(⋃ L(g), d) )
```

with `combine` the table of §15.2. If any member may match below `d`, the union may: it MUST NOT report `Pruned` unless every member does. A negated member (JS) contributes its own match and may-below (§15.4).

All-below is a property of the union's language (§15.5): one covering member is enough, and members that each cover a part cover together (`a/*` the children, `a/*/**` everything deeper).

### 16.3 Literal acceleration

An implementation MAY use literal facts (prefix, suffix, substring, exact) to reject paths before running the full matcher, for a single pattern as for a union. See [`../theory/03-literal-prefilter.md`](../theory/03-literal-prefilter.md) and [`../theory/04-walker-and-pruning.md`](../theory/04-walker-and-pruning.md) for the facts the reference implementations build on.

---

## 17. Static Prefixes

`static_prefixes(P)` is a list of byte strings such that every path in `L(P)` starts with one of them, where "starts with" means byte for byte, with `/` as the separator, and either the whole path or a prefix followed by a separator. Walkers seed traversal from them (WALKER_SPEC.md §6).

The reference implementations compute them from the brace expansions of §10.1: for each expansion, its literal head up to its last whole segment, with heads a shorter one covers removed. An expansion with no literal head gives the empty prefix `""`, which covers every other head; so does a negated pattern or union member, since its matches can fall anywhere. The list is in the pattern's case (a case-insensitive match may differ there, §13) and holds `/` only (a Windows path with `\` may differ there, §14.4). An expansion that can match nothing gives no head: on Windows, one through an escaped `\` (§4.2), so a pattern all of whose expansions are dead has an empty list.

The work is bounded linearly in the pattern; past the bound a head ends at its last whole segment, which is a shorter prefix and still holds.

---

## Appendix A: Differences from Other Tools

| Behavior                                      | globstar                            | Bash               | picomatch                       | globset                      | fast-glob (npm)               | fast-glob (crate)              | doublestar                                     |
| --------------------------------------------- | ----------------------------------- | ------------------ | ------------------------------- | ---------------------------- | ----------------------------- | ------------------------------ | ---------------------------------------------- |
| `dot` default                                 | `true` (matcher) / `false` (walker) | `false`            | `false`                         | `true` (no concept)          | `false`                       | `true` (no concept)            | `true` (`Glob`: `WithNoHidden()`)              |
| Case-insensitive                              | ASCII only, default `false`         | `shopt nocaseglob` | `nocase` option                 | `case_insensitive` builder   | `caseSensitiveMatch: false`   | ❌                              | `WithCaseInsensitive()` (`Glob` only), Unicode |
| `**` must own a segment                       | enforced                            | ✅                  | ✅                               | ✅ (with `literal_separator`) | ✅                             | ✅                              | ✅                                              |
| `a/**` matches `a`                            | ❌                                   | ⚠️ (lists `a/`)    | ✅ (❌ with `strictSlashes`)      | ❌                            | ⚠️ (walker: no; matcher: yes) | ❌                              | ✅                                              |
| `**` in braces judged on the expansion (§10.1) | ✅                                   | ✅ (expands first)  | ❌ (branch-local `.*`)           | ❌                            | ✅ (expands first)             | ❌                              | ⚠️ (not `**{/a,b}`)                            |
| `**` in a single-branch brace `{**}` (§10.4)  | degrades to `*`                     | degrades to `*`    | keeps globstar                  | a group; `**` acts as `*`    | keeps globstar                | a group (= `**`)               | a group (= `**`)                               |
| `**/X` matches the absolute `/a/X` (§9.4)     | ✅                                   | n/a                | ✅                               | ✅                            | n/a (walker)                  | ✅                              | ✅                                              |
| Extglobs                                      | ❌ (literal)                         | ✅ (`shopt`)        | ✅                               | ❌                            | ✅                             | ❌                              | ❌                                              |
| POSIX character classes                       | ❌                                   | ✅                  | ✅                               | ❌                            | ✅                             | ❌                              | ❌                                              |
| `[/]` inside a class (§8.2)                   | parse error                         | literal            | ⚠️ (`/` or the literal text)    | matches `/`                  | never matches                 | never matches                  | ⚠️ (`Match`: `/`; `Glob`: error)               |
| POSIX first-`]` rule (§8.5)                   | ✅                                   | ✅                  | ✅                               | ✅                            | ✅                             | ✅                              | ❌ (error)                                      |
| `[!a]` same as `[^a]`                         | ✅ (both negate)                     | ✅                  | ❌ (only `[^a]`; ✅ with `posix`) | ✅                            | ✅                             | ✅                              | ✅                                              |
| Brace ranges `{1..10}`                        | ❌                                   | ✅                  | ✅                               | ❌                            | ✅                             | ❌                              | ❌                                              |
| Brace nesting                                 | ≤ 32                                | ✅                  | ✅                               | ✅                            | ✅                             | ≤ 10                           | ✅                                              |
| Leading-`!` negation                          | ✅                                   | n/a                | ✅                               | ❌ (literal)                  | ✅ (an ignore pattern)         | ✅                              | ❌ (literal)                                    |
| `/` required in patterns                      | ✅                                   | n/a                | ✅                               | ⚠️                           | ✅                             | ✅                              | ⚠️ (`PathMatch`)                               |
| `\` is always the escape                      | ✅                                   | ✅                  | ✅                               | ⚠️                           | ✅                             | ✅ ¹                            | ⚠️ (`PathMatch`)                               |
| `\` in a path is a separator (§14.2)          | platform-conditional                | ❌                  | ⚠️ (`windows` option)           | platform                     | n/a (walker)                  | platform ¹                     | platform (`PathMatch`)                         |
| Stray `]`, `}`, `)`                           | literal                             | literal            | literal                         | `}` is an error              | literal                       | literal                        | ⚠️ (`}`: `Glob` error)                         |
| Unclosed constructs                           | parse error                         | literal            | ⚠️                              | error                        | ⚠️ (no error)                 | no match (`validate()` errors) | error                                          |
| `match_dir` API                               | ✅ (first-class)                     | n/a                | ❌                               | ❌                            | ❌                             | ❌                              | ❌                                              |
| `*` and `**` match `.` and `..` (dot on)      | ✅                                   | ❌                  | ❌                               | ✅                            | ❌                             | ✅                              | ✅                                              |

Symbols: ✅ yes, ❌ no, ⚠️ partial or conditional, n/a not applicable. ¹ Measured on Unix only; the crate has no platform switch for `\` in patterns and splits paths with `std::path::is_separator`.

Every cell was measured on 2026-10-16: Bash 5.2.37 (filename expansion with `globstar`, and `[[ ]]` where no file can exist), picomatch 4.0.7 (4.0.4 and 2.3.2 answer the same), globset 0.4.20 (with `literal_separator(true)`), fast-glob 3.3.3 from npm (a walker; it matches through micromatch 4.0.8 and picomatch 2.3.2, and expands braces first), the fast-glob crate 1.1.2 (`glob_match`), and doublestar v4.10.3 (`Match`, and `Glob` on real trees). Windows cells come from a simulated platform. Tools change between versions.

> **Note.** Of these tools, picomatch is the closest. globstar is stricter about input (`/` is mandatory in patterns), treats `match_dir` as a first-class operation, and is stricter about syntax errors: an unclosed class or brace is a hard error, never silently literal.

---

## Appendix B: Glossary

| Term                   | Meaning                                                                                                                  |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Pattern                | The user-supplied glob string, read as UTF-8 bytes (§2.1).                                                               |
| Path                   | The byte sequence being matched (§2.1).                                                                                  |
| `Seps`                 | The implementation-defined set of separator bytes in a path (§2.2). `/` is always in it.                                 |
| Segment                | A run of non-separator bytes between separators; a path is segments joined by single separators (§2.3).                  |
| Segment start          | Position 0, or a position right after a separator (§2.3).                                                                |
| `L(P)`                 | The set of paths a pattern denotes under the options in force (§2.4).                                                    |
| Globstar               | A `**` that owns a whole segment (§9.1). Any other `**` is a star.                                                       |
| Expansion              | One pattern obtained by choosing a branch in every brace (§10.1); a pattern means the union of its expansions.            |
| Dot-protected position | A `.` at a segment start, under `dot = false` (§12). No wildcard consumes it.                                             |
| Wildcard               | `*`, `?`, `**` and a negated class: the constructs dot protection restricts (§12).                                       |
| `match_dir` results    | `Match`, `Pruned`, `Descend`, `DescendAndMatch`, `DescendAll`, `DescendAllAndMatch` (§15).                               |
| All-below              | Every path below a directory matches (§15.5). The two `DescendAll…` results carry it.                                    |
| Union                  | The boolean OR of N patterns: `Glob::union` in Rust, `globstar(patterns)` and `compileMatcher(patterns)` in JS (§16).    |
| Literal facts          | Byte facts every match of a pattern must satisfy (prefix, suffix, substring, exact), used to reject paths early (§16.3). |
| Static prefixes        | Literal heads every match starts with, one per expansion up to its last whole segment (§17). Walkers seed from them.     |
