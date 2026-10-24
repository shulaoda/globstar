# GLOB_SPEC: The Glob Pattern Language

**Status.** This document is the normative definition of the glob dialect implemented by the `globstar` Rust crate and the `@globstar/core` JS package. Where code, tests or other documents disagree with it, the code is wrong.

**Conformance.** The executable form of this document is the corpus in `fixtures/corpus/`: one row per pattern, path and expected answer. Both implementations run every row. A behavior is specified only where this document states it or a row pins it.

**Conventions.** The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119 and state requirements on an implementation or on a caller. Sentences without a key word define the language. Paragraphs headed *Remark* and all examples are informative.

**Notation.** Σ is the set of bytes `0..255`. Σ* is the set of finite byte strings and ε the empty string. `u · v` is the concatenation of `u` and `v`. For a pattern `P`, `L(P) ⊆ Σ*` is the set of paths it denotes (§2.4). `Seps ⊆ Σ` is the separator set (§2.2).

## Contents

1. [Scope](#1-scope)
2. [Model](#2-model)
3. [Options](#3-options)
4. [Lexical Rules](#4-lexical-rules)
5. [Grammar and Equivalences](#5-grammar-and-equivalences)
6. [Errors and Limits](#6-errors-and-limits)
7. [Literals, `?`, `*` and `/`](#7-literals---and-)
8. [Character Classes](#8-character-classes)
9. [The Globstar `**`](#9-the-globstar-)
10. [Braces](#10-braces)
11. [Negation](#11-negation)
12. [Dot Protection](#12-dot-protection)
13. [Case Folding](#13-case-folding)
14. [Matching a Path](#14-matching-a-path)
15. [Directory Matching](#15-directory-matching)
16. [Unions](#16-unions)
17. [Static Prefixes](#17-static-prefixes)

- [Appendix A: Differences from Other Tools](#appendix-a-differences-from-other-tools)
- [Appendix B: Glossary](#appendix-b-glossary)

---

## 1. Scope

### 1.1 In scope

- The pattern language: `*`, `**`, `?`, character classes `[...]`, braces `{...}`, the escape `\` and leading-`!` negation.
- Matching: the value of `is_match(P, path)` for every pattern `P` and path (§2–§14).
- Directory matching: the operation `match_dir` and the meaning of its six results (§15).
- Unions: the disjunction of several patterns (§16).
- Static prefixes: the literal heads every match of a pattern starts with (§17).

Filesystem traversal is the subject of [WALKER_SPEC.md](./WALKER_SPEC.md).

### 1.2 Out of scope

The following constructs are not part of the language. Where their syntax can occur in a pattern, the bytes are ordinary literals.

- Bash extglobs `@()`, `?()`, `*()`, `+()`, `!(...)`. The bytes `(`, `)`, `@`, `+` and `|` are literal; a `?` or `*` inside keeps its wildcard meaning, so `@(a|b)` matches only the path `@(a|b)`. Alternation is written with a brace (§10) and negation with the leading `!` (§11).
- POSIX character classes `[[:alpha:]]`, collating elements `[[.a.]]` and equivalence classes `[[=a=]]` (§8.7).
- Bash brace ranges `{1..10}`, `{a..z}`.
- Regular-expression features: backreferences, groups, lookaround.
- Gitignore re-inclusion, where a later rule cancels an earlier one. This belongs to a walker.
- Unicode case folding. Only ASCII folding is defined (§13).
- Shell expansions `~` and `$VAR`.

### 1.3 Compatibility

The closest existing dialect is picomatch's. Every deliberate difference from it and from other tools is listed in Appendix A.

---

## 2. Model

### 2.1 Bytes

Matching is defined over bytes.

- A pattern is a Unicode string, read as its UTF-8 bytes. A string with no UTF-8 form (a JS string with a lone surrogate) is not a pattern, and an implementation MUST reject it.
- A path is an arbitrary byte string. An implementation whose paths are strings encodes them as UTF-8; a lone surrogate encodes as U+FFFD.
- Only ASCII bytes carry a meta meaning. Every byte outside ASCII is literal.
- Comparison is byte for byte, and every construct consumes bytes, not characters (§14.3).

### 2.2 Separators

In a pattern, `/` is the only separator, on every platform. `\` is the escape (§4.2) and never a separator.

| Pattern        | Separator | Reading                                            |
| -------------- | --------- | -------------------------------------------------- |
| `src/main.rs`  | yes       | two segments                                       |
| `src\main.rs`  | no        | one segment, `srcmain.rs`, on every platform       |
| `src\\main.rs` | no        | one segment, `src\main.rs`, holding a literal `\`  |

A tool that accepts native paths as patterns MUST normalize them to `/` first.

In a path, a separator is any byte of a set `Seps`. `/ ∈ Seps` on every platform. The other members of `Seps` are the implementation's choice and SHOULD follow the platform: the reference implementations put `\` in `Seps` on Windows and nowhere else.

Wherever this document asks whether a path byte is a separator, it asks whether the byte is in `Seps`: when a pattern `/` consumes a byte (§7.4), when `**` crosses segments (§9), when a segment start is located (§2.3) and when a class refuses a separator (§8.2). §14.2 collects the consequences.

### 2.3 Segments

A path is a sequence of segments joined by single separator bytes. A segment is a possibly empty run of non-separator bytes. Position `i` of a path is a *segment start* when `i = 0` or `path[i − 1] ∈ Seps`. A segment is *dot-led* when its first byte is `.`.

A pattern is likewise read as segments between its `/` bytes. Every construct but the globstar (§9) lies within one segment: `*`, `?` and a class never consume a separator, and a brace branch holds a `/` only as a pattern separator of its own.

### 2.4 The language of a pattern

A pattern `P` denotes a set of paths `L(P)`, and

```
is_match(P, path)  :=  path ∈ L(P)
```

A match covers the whole path: `P` consumes every byte of the path, and nothing is matched as a substring (§14.1). `L(P)` depends on the two options of §3; `L(P)` is always read under the options in force.

The language is compositional. Each construct of §7–§10 denotes a set of byte strings, a sequence denotes the concatenation of its parts' languages, the leading `!` (§11) complements, and two cross-cutting rules, dot protection (§12) and case folding (§13), restrict or widen what the wildcards consume.

---

## 3. Options

Matching takes two options. Both are parameters of `L(P)`, not of syntax: parsing does not depend on them.

| Option             | Default (matcher) | Default (walker) | Defined in |
| ------------------ | ----------------- | ---------------- | ---------- |
| `dot`              | `true`            | `false`          | §12        |
| `case_insensitive` | `false`           | `false`          | §13        |

### 3.1 `dot`

Whether a wildcard may consume a `.` that starts a segment (§12).

| Setting       | `*.txt` on `.txt` | `**/*` on `a/.b` | To match a dotfile                               |
| ------------- | ----------------- | ---------------- | ------------------------------------------------ |
| `dot = false` | no match          | no match         | name the dot (`.*`, `[.]*`) or set `dot = true`  |
| `dot = true`  | match             | match            |                                                  |

`dot = false` is the picomatch behavior. Bash and POSIX `fnmatch` are stricter: there a class never matches a leading dot, and only a literal `.` does.

Under `dot = true`, `*` and `**` also match the names `.` and `..`: `public/**` matches `public/../secret.txt`. A path is matched as the bytes given and never resolved (§14.2); a caller that matches paths from outside normalizes them first. A walker never meets these names, since directory listings omit them.

*Remark.* Hiding dotfiles is a filesystem convention, not a property of the pattern language. The matcher therefore defaults to `dot = true`, and the walker, which reads filesystems, to `dot = false`.

### 3.2 `case_insensitive`

Whether ASCII letters compare case-folded (§13).

### 3.3 No other options

Every construct of the language is always in force. Brace expansion, the globstar and the escape cannot be disabled; a literal brace is written `\{a,b\}`. Unicode case folding is not an option: a caller who needs it normalizes pattern and path first (§13).

---

## 4. Lexical Rules

### 4.1 Meta bytes and scopes

The meta bytes are `* ? [ ] { } , \`, and `!` at the start of the pattern. Every other byte, and every non-ASCII byte, is literal. The meta bytes form three groups.

| Group        | Bytes     | Rule                                                                                                                       | Literal form                 |
| ------------ | --------- | -------------------------------------------------------------------------------------------------------------------------- | ---------------------------- |
| Always meta  | `* ?`     | always a wildcard                                                                                                          | `\*`, `\?`                   |
| Scoped       | `[ ] { }` | an opener starts a scope that MUST be closed; a closer is meta only inside its open scope; `,` separates branches only inside a brace | `\[`, `\]`, `\{`, `\}`, `\,` |
| Prefix       | `!`       | negates only as a leading run (§4.3, §11); elsewhere literal                                                               | `\!`                         |

- `[` opens a class and MUST be closed; otherwise the pattern is the error `UnterminatedClass` (§6.1).
- `{` opens a brace and MUST be closed; otherwise `UnterminatedBrace`.
- `]` and `}` with no open scope are literal: `a]b` matches `a]b`, and `{a,}}` is the brace `{a,}` followed by a literal `}`, matching `a}` and `}`.
- `|`, `@`, `+`, `(` and `)` have no meaning: `file(1).txt` and `@(a|b)` are literal (§1.2).

### 4.2 The escape `\`

`\` is the escape character everywhere in a pattern, in classes and braces included. Its meaning depends on the byte `X` after it.

| Next byte          | Result                                                                     |
| ------------------ | -------------------------------------------------------------------------- |
| `/`                | the error `EscapedSeparator`                                               |
| end of the pattern | the error `TrailingBackslash`                                              |
| any other `X`      | the literal byte `X`, whether `X` is a meta byte (`\*`) or not (`\a` ≡ `a`) |

`\\` is a literal `\`. A literal never matches a separator byte (§7.1), so on a platform where `\ ∈ Seps` an escaped backslash matches nothing (§14.4).

*Remark.* No file name on any platform contains a `/`, so an escaped separator could never match. Reporting it is more useful than a pattern that matches nothing.

### 4.3 The leading `!`

A run of `!` bytes at position 0 is read before anything else.

1. The run is counted.
2. An odd count is one negation (§11); an even count is none.
3. The body starts at the first byte after the run; every later `!` is literal.

The run belongs to the pattern as a whole: it is read before braces (§10.1), so `{!a,b}` holds no negation, and no option disables it (§11.4). A body that is empty after the run (`!`, `!!`) is the error `Empty` (§6.1).

### 4.4 Scopes

A class or brace is read from its opener to its own closer by its own rules (§8.3, §10), without regard to the enclosing text. The first opener owns the input, so a closer inside an inner scope is invisible to the outer one.

| Pattern  | Reading                                                                         | Result              |
| -------- | ------------------------------------------------------------------------------- | ------------------- |
| `[{]}`   | the class holds `{` and closes at `]`; the `}` is literal                       | matches `{}`        |
| `{[}]`   | the brace opens; the class takes `}` as a member and closes at `]`; no `}` left | `UnterminatedBrace` |
| `[}{]`   | the class holds `}` and `{`                                                     | matches `}` or `{`  |
| `{a,[]}` | the brace opens; the class takes `]` as its first member; no second `]` follows | `UnterminatedClass` |
| `[(])`   | the class holds `(`; the `)` is literal                                         | matches `()`        |

This is the model of bash brace expansion, POSIX bracket expressions and regular expressions. A pattern that is balanced to the eye but crossed in structure, such as `{[}]`, is an error rather than literal text.

### 4.5 Length

A pattern longer than 65,536 bytes of UTF-8 is the error `TooLong` (§6.1).

---

## 5. Grammar and Equivalences

### 5.1 Grammar

```bnf
Pattern       ::= [Negation] Sequence       ; the Sequence is not empty (§6.1, Empty)
Negation      ::= "!"+                      ; leading run; its parity decides (§4.3, §11)
Sequence      ::= Atom*
Atom          ::= Literal | Separator | AnyChar | Star | Globstar
               |  CharClass | Brace | Escaped

Literal       ::= a byte with no meta role at this position
Separator     ::= "/"
AnyChar       ::= "?"
Star          ::= "*"                       ; when not part of a Globstar; a run of stars is one Star (§5.2)
Globstar      ::= "**"                      ; exactly two, where they own a segment (§9.1)

CharClass     ::= "[" ("^" | "!")? CharClassItem+ "]"
CharClassItem ::= Literal | Range | Escaped
Range         ::= RangeEnd "-" RangeEnd     ; a first-position `]` can start one (§8.5);
                                            ; a `-` right after a Range is a member
RangeEnd      ::= Literal | Escaped         ; `[\a-c]` ≡ `[a-c]`

Brace         ::= "{" Sequence ("," Sequence)+ "}"
                                            ; `{...}` with no comma is literal (§10.4);
                                            ; a branch may be empty (§10.3)
Escaped       ::= "\" AnyByte               ; `\X` is the literal byte X (§4.2)
```

### 5.2 Equivalences

The following spellings denote the constructs on the right. They change nothing about the language; they state which construct a spelling is.

| Spelling                                                    | Reads as                    | Rule                                                                                    |
| ----------------------------------------------------------- | --------------------------- | --------------------------------------------------------------------------------------- |
| a run of stars `***`                                        | one `*`                     | a star run inside a segment is one star                                                 |
| `**` that does not own a segment (`a**b`, `a/**b`, `a**/b`) | `*` (`a*b`, `a/*b`, `a*/b`) | §9.1                                                                                    |
| `**/**/`                                                    | one `**/`                   | §9.6                                                                                    |
| `{a}` (no comma)                                            | the literal `{a}`           | §10.4; the braces are plain bytes, so a `**` beside them is a star                      |
| `\X` with an ordinary `X`                                   | `X`                         | §4.2                                                                                    |
| `[a-]`, `[-a]`                                              | `a` or `-`                  | §8.3: a `-` at the edge of a class is a member                                          |
| `a//b`                                                      | as written                  | each `/` is a strict separator (§7.4); only the boundaries of a `**` are lenient (§9.5) |

Beside a brace, whether a `**` owns a segment is judged on each expansion (§10.1).

---

## 6. Errors and Limits

### 6.1 Compile-time errors

Compilation MUST reject exactly the following inputs. It has no other failure mode: a pattern either compiles or raises one of these.

| Error                 | Example                    | Rule                                                              |
| --------------------- | -------------------------- | ----------------------------------------------------------------- |
| `Empty`               | `""`, `!`, `!!`            | no pattern to match or to negate (§4.3)                           |
| `TooLong`             | more than 65,536 bytes     | §4.5                                                              |
| `UnterminatedClass`   | `[abc`, `[]`, `[/]`        | an opener needs its closer (§4.1); a `/` ends the segment (§8.2); a first `]` is a member (§8.5) |
| `UnterminatedBrace`   | `{a,b`                     | an opener needs its closer (§4.1)                                 |
| `TrailingBackslash`   | `foo\`                     | nothing to escape (§4.2)                                          |
| `EscapedSeparator`    | `a\/b`                     | §4.2                                                              |
| `InvalidRange`        | `[z-a]`                    | the end of a range is below its start (§8.1)                      |
| `BraceNestingTooDeep` | 33 nested `{`              | §6.2                                                              |
| `EmptyPatternSet`     | a union of no patterns     | §16.1                                                             |
| `NegatedInUnion`      | a union member `!b`        | an implementation that rejects negated union members (§16.1)      |

A stray `]`, `}`, `(` or `)` is not an error but a literal byte (§4.1). The spellings of §5.2 are not errors.

### 6.2 Limits

Two limits are resource guards and not rules of the language. A pattern inside them means what §7–§13 say; a pattern outside them is refused.

- Length: 65,536 bytes (§4.5).
- Nesting: an implementation MUST accept braces nested up to 32 levels deep and MUST reject deeper nesting with `BraceNestingTooDeep`. The depth counts every `{` as it opens, including one that turns out to be literal (§10.4). Braces side by side do not nest.

Nothing else is bounded. An implementation MUST compile every pattern within these limits, whatever its braces: the number of branches, the breadth of a product `{a,b}{c,d}…`, braces beside a `**`, and braces whose branches cross a `/` and multiply the segments of the pattern (`{a,b,/}` repeated `k` times has about `k²` distinct segments). Compile time and memory SHOULD be linear in the length of the pattern.

### 6.3 Reporting

An error names its kind as in §6.1 and, where it has a position, the byte offset into the UTF-8 pattern at which it was found (for `InvalidRange`, where the range starts). An error raised by one member of a union identifies the member by its position in the list, counting every member given (§16.1).

---

## 7. Literals, `?`, `*` and `/`

### 7.1 Literal

A literal byte matches that byte, once. A literal that is a separator byte on the platform (`\` on Windows, §2.2) matches nothing: a pattern `/` is the only construct that consumes a separator.

### 7.2 `?`

`?` matches exactly one byte that is not in `Seps`. It does not match the empty string, and it consumes one byte, not one character (§14.3).

### 7.3 `*`

`*` matches zero or more bytes not in `Seps`. It matches the empty string (`foo*bar` matches `foobar`) and never crosses a separator (`src/*` does not match `src/a/b`). A run of stars inside a segment is one star (§5.2).

### 7.4 `/`

A pattern `/` consumes exactly one separator byte of the path: `a/b` does not match `a//b`, and `a//b` requires two separators. The only places where a run of separators is accepted belong to the globstar (§9.5).

---

## 8. Character Classes

A class is one construct that consumes one byte chosen from a set.

### 8.1 Forms

| Form       | Kind    | Matches                                |
| ---------- | ------- | -------------------------------------- |
| `[abc]`    | set     | `a`, `b` or `c`                        |
| `[a-z]`    | range   | any byte from `a` to `z` inclusive     |
| `[abc0-9]` | mixed   | a member of the set or of the range    |
| `[!abc]`   | negated | any byte that is none of `a`, `b`, `c` |
| `[^abc]`   | negated | the same as `[!abc]`                   |

A range whose end is below its start (`[z-a]`) is the error `InvalidRange` (§6.1). The comparison is byte-level and case-sensitive under every option (§13).

### 8.2 A class is segment-local

A class closes inside the segment that contains it. A `/` in a class, written directly (`[/]`, `[a/b]`, `[^/]`) or escaped (`[\/]`), ends the segment before a closer was found, and the pattern is the error `UnterminatedClass`.

At match time a class never matches a member of `Seps`, whatever its polarity. Since `/ ∈ Seps` always, the rule adds something only where `\ ∈ Seps` (Windows, §14.4): there `[^abc]` rejects `\`, and the positive class `[\\]` never matches. On Unix, `\` is an ordinary byte and `[\\]` matches it.

### 8.3 Bytes inside a class

| Byte                                   | Role            | Where                                                                                   |
| -------------------------------------- | --------------- | --------------------------------------------------------------------------------------- |
| `]`                                    | closer          | anywhere but the first position (§8.5)                                                  |
| `-`                                    | range separator | between two members, a first-position `]` included; a member next to `[` or the closer, or right after a range |
| `!`, `^`                               | negation marker | immediately after `[`                                                                   |
| `\`                                    | escape          | anywhere; the next byte becomes a literal member (§4.2)                                 |
| `/`                                    | not allowed     | anywhere: `UnterminatedClass` (§8.2)                                                    |
| `*`, `?`, `{`, `}`, `(`, `)`, `@`, `+` | literal         | meta bytes outside a class are plain members inside                                     |
| any other byte                         | literal         |                                                                                         |

Escapes inside a class follow §4.2: `\X` is the member `X`, whatever `X` is. So `[\?]` holds `?`, `[\-]` holds `-` and is no range, `[\!]` is a positive class holding `!`, and `[\]]` holds `]`, because the escape consumes the first `]` and the second closes.

| Pattern            | Result                  | Rule                                                 |
| ------------------ | ----------------------- | ---------------------------------------------------- |
| `[?*!(]`           | class `{?, *, !, (}`    | meta bytes are plain members inside                  |
| `[a-z]`            | range `a..z`            | `-` between two members                              |
| `[a\-z]`           | class `{a, -, z}`       | an escaped `-` is a member                           |
| `[\a-c]`, `[a-\c]` | range `a..c`            | an escaped byte can end a range                      |
| `[a-c-e]`          | class `{a..c, -, e}`    | a `-` right after a range is a member                |
| `[]-a]`            | range `]..a`            | a first-position `]` can start a range (§8.5)        |
| `[-abc]`, `[abc-]` | class with `-`          | `-` next to a bracket is a member                    |
| `[!abc]`           | negated `{a, b, c}`     | `!` in first position                                |
| `[\!abc]`          | positive `{!, a, b, c}` | the escape overrides the negation marker             |
| `[]]`              | class `{]}`             | §8.5                                                 |
| `[!]]`             | negated `{]}`           | a first `]` after the negation marker                |
| `[\]]`             | class `{]}`             | escaped form                                         |
| `[\]`              | `UnterminatedClass`     | the escape consumes the only `]`                     |
| `[a\`              | `TrailingBackslash`     | the pattern ends inside the escape                   |
| `[]`               | `UnterminatedClass`     | the first `]` is a member; no closer follows         |
| `[/]`, `[\/]`      | `UnterminatedClass`     | a separator in a class (§8.2)                        |
| `[\\]`             | class `{\}`             | matches `\` on Unix; never matches on Windows (§8.2) |

### 8.4 Negated classes and `dot`

A negated class is a wildcard for the purpose of dot protection (§12): under `dot = false`, `[!a]` at a segment start does not match `.`; under `dot = true` it does. A positive class that lists `.` (`[.]`, `[.a]`) matches `.` under either setting, because it names the dot.

### 8.5 The first `]`

A `]` immediately after `[`, `[!` or `[^` is a member, not the closer, as in bash, fnmatch, picomatch, globset and fast-glob.

- `[]]` matches `]`; `[!]]` matches any byte but `]`; `[]-]` matches `]` or `-`.
- A first `]` can start a range: `[]-a]` is the range `]..a`, which holds `^`, `_` and `` ` ``.
- `[]`, `[!]` and `[^]` are `UnterminatedClass`: the `]` was a member, and no closer follows.
- `[` alone is `UnterminatedClass`.
- `[a-]` is the class `{a, -}`: a trailing `-` is a member.

### 8.6 A class consumes one byte

Like every construct (§14.3), a class matches one byte. A non-ASCII character inside a class stands for its UTF-8 bytes, each a member of its own, and a range with a non-ASCII end ranges over bytes: `[é]` is the class `{0xC3, 0xA9}` and never matches `é`, which is two bytes. One of several characters is matched with a brace: `{é,è}`.

### 8.7 POSIX classes

`[[:alpha:]]` is the class `[[:alpha:]`, with the members `[`, `:`, `a`, `l`, `p` and `h`, followed by a literal `]`. It matches `a]` and never a letter on its own (§1.2).

---

## 9. The Globstar `**`

`**` is the only construct that may consume a separator. That is its only difference from `*`.

### 9.1 A globstar owns a segment

`**` is a globstar if and only if it is preceded by `/` or the start of the pattern and followed by `/` or the end of the pattern. Otherwise it is two stars, which collapse to one (§5.2).

| Pattern       | Globstar | Reads as |
| ------------- | -------- | -------- |
| `src/**/*.ts` | yes      |          |
| `**/*.ts`     | yes      |          |
| `**`          | yes      |          |
| `src/**`      | yes      |          |
| `a**b`        | no       | `a*b`    |
| `a/**b`       | no       | `a/*b`   |
| `a**/b`       | no       | `a*/b`   |

Beside a brace the test is made on each expansion (§10.1). The neighbor of a `**` beside a brace is the first or last token of each branch, or what lies beyond the brace when the branch is empty; the neighbor of a `**` at the edge of a branch is what surrounds the brace. The same `**` can be a globstar in one expansion and a star in another.

| Pattern      | Expansions       | Remark                                                            |
| ------------ | ---------------- | ----------------------------------------------------------------- |
| `a{**,x}b`   | `a*b` ∪ `axb`    | a star                                                            |
| `{**,x}/b`   | `**/b` ∪ `x/b`   | matches `b`                                                       |
| `{a/**,x}c`  | `a/*c` ∪ `xc`    | a star                                                            |
| `a/{**/x,y}` | `a/**/x` ∪ `a/y` | a globstar with the lenient boundary of §9.5; matches `a//x`      |
| `**{/a,b}`   | `**/a` ∪ `*b`    |                                                                   |
| `{a/,b}**`   | `a/**` ∪ `b*`    |                                                                   |
| `{a,}**`     | `a*` ∪ `**`      | the empty branch leaves the `**` alone                            |

### 9.2 Language

A globstar matches any byte string, separators and the empty string included.

```
L(**)      = Σ*
L(a/**)    = { "a/" · w : w ∈ Σ* }                       ; "a/" is a member
L(**/b)    = { w · "b" : w ∈ Σ*, w = ε or w ends in / }
L(a/**/b)  = { "a/" · w · "b" : w = ε or w ends in / }   ; a/b, a/x/b, a/x/y/b, …
```

Under `dot = false` every segment start inside a globstar is dot-protected (§12): `**` matches neither `a/.b` nor `.a/b`.

### 9.3 `a/**` and the forms of `a`

| Path    | `is_match` | Reason                     |
| ------- | ---------- | -------------------------- |
| `a`     | false      | the prefix `a/` is missing |
| `a/`    | true       | `a/` and the empty suffix  |
| `a/b`   | true       |                            |
| `a/b/c` | true       |                            |

This agrees with globset, minimatch and the fast-glob crate, and differs from picomatch (without `strictSlashes`) and doublestar, where `a/**` matches `a`. A directory and everything below it is selected by `{a,a/**}`. §15.3 gives the `match_dir` answers for the same forms.

### 9.4 A leading `**/`

A pattern that begins with `**/X` means "`X` at any depth" and accepts every form of root: relative paths (`foo`, `a/foo`), Unix absolute paths (`/foo`, `/a/foo`), UNC paths (`//server/share/foo`) and drive-letter paths (`C:/a/foo`; `C:` is an ordinary segment).

```
L(**/X) = L( (Sep | seg Sep)* X )
```

where `Sep` is one separator byte and `seg` a non-empty run of non-separator bytes: an optional run of root separators, then zero or more whole segments. A pattern that begins with `/**/X` requires the absolute form.

| Pattern | `/` | `/b` | `a/b` | `b` |
| ------- | --- | ---- | ----- | --- |
| `**`    | yes | yes  | yes   | yes |
| `**/`   | yes | no   | no    | no  |
| `**/b`  | no  | yes  | yes   | yes |
| `/**/b` | no  | yes  | no    | no  |

`**/*.rs` therefore means every `.rs` file, relative or absolute; `/**/*.rs` restricts the match to absolute paths.

With braces the rule follows from §10.1. In `{**/a,README}` only the expansion `**/a` begins with `**/`, so the pattern matches `/a` and not `/README`. A `**/` that is not at the start of the pattern (`a/{**/x,y}`) has the lenient boundary of §9.5 and nothing more.

### 9.5 Lenient separators at a globstar

A pattern `/` consumes one separator byte (§7.4). A run of separators is accepted in exactly two places, both at a globstar:

- the `/` on either side of a `**`: `a/**/b` matches `a//b`, as in picomatch and globset. This holds across braces: `{a,/**}/{**/x,b}` matches `a//x`;
- a `**/` at the start of the pattern, which also accepts zero or more leading separators (§9.4): `**/foo` matches `foo`, `/foo` and `//server/share/foo`.

### 9.6 Consecutive and independent globstars

`**/**/` is one `**/` (§5.2); the language is unchanged. Two globstars in one pattern are independent, in different branches of a brace as well: in `{src/**,lib/**}` neither is merged with or bound to the other.

---

## 10. Braces

### 10.1 The expansion equation

A brace denotes a union. For any fragments `pre` and `post` and branches `A`, `B`, …:

```
L( pre{A,B,…}post ) = L( pre A post ) ∪ L( pre B post ) ∪ …
```

Each expansion is read as a pattern of its own under every rule of this document. In particular, whether a `**` owns a segment (§9.1) is judged on the expansion. A brace neither gives a `**` a power it lacks outside (`a{**,x}b` ≡ `a*b` ∪ `axb`, since the expanded neighbors are `a` and `b`) nor takes away a power it has (`{**,x}/b` ≡ `**/b` ∪ `x/b`, which matches `b`). The same holds for the lenient boundary (§9.5), the leading `**/` (§9.4) and dot protection (§12): `x{/,y}{**,a}/z` contains the expansion `x/**/z` and matches `x//z`.

Two rules bound the equation:

1. The leading `!` is read before expansion (§4.3). `{!a,b}` expands to the body `!a`, whose `!` is literal.
2. An empty expansion denotes ε, not an error. The empty branch of `{a,}` contributes the empty string; the error `Empty` (§6.1) applies to the pattern as written.

The equation operates on tokens, not bytes. A branch is spliced in as a unit, and tokens never fuse across the splice: `*{,a}*` is `L(* ε *) ∪ L(* a *)`, and the two stars of the empty expansion remain two stars, which collapse to one (§5.2). They do not form a `**`.

### 10.2 Forms

| Form         | Matches                                   |
| ------------ | ----------------------------------------- |
| `{a,b,c}`    | `a`, `b` or `c`                           |
| `{a*b,c?}`   | a branch may hold wildcards               |
| `{a,{b,c}}`  | nested braces: `a`, `b` or `c`            |
| `{a,b}{c,d}` | a product: `ac`, `ad`, `bc`, `bd`         |

### 10.3 Empty branches

`{a,}` is legal and denotes `a` or the empty string.

### 10.4 Single-branch braces

`{a}`, with no comma, is not a brace. It is the three literal bytes `{a}`, as in bash, picomatch and the npm fast-glob. (globset, doublestar and the fast-glob crate read it as a group, so there `{a}` matches `a`.)

The literal `{` or `}` is a plain byte and no segment boundary, so a `**` beside it is a star (§9.1): `{**}` ≡ `{*}` matches `{a}` and not `{a/b}`, and `{a/**}` ≡ `{a/*}`. A `**` with `/` on both sides stays a globstar: `{a/**/b}` matches `{a/x/y/b}`. This differs from picomatch and the npm fast-glob, which keep the globstar, and agrees with bash (Appendix A).

A separator stays structural and never becomes literal content: `x{a/b}y` is the two-segment pattern `x{a` / `b}y`, exactly like `x\{a/b\}y`.

*Remark.* Brace expansion requires alternatives: in bash, `echo {a}` prints `{a}` and `echo {a,b}` prints `a b`. A file name with braces is matched by writing them bare (no comma) or escaped (`\{a\}`). Classes differ: `[a]` is a class, since bracket expressions never had a minimum number of members.

### 10.5 Cost

The cost of compiling and matching a pattern is bounded by the pattern's length (§6.2), not by the number of its expansions. A product `{a,b}{c,d}{e,f}…`, or braces whose branches cross a `/`, does not multiply the work of an implementation.

### 10.6 Bounds

An unclosed brace (`{a,b`) is `UnterminatedBrace`. Nesting is bounded at 32 levels (§6.2). Nothing else about braces is bounded.

### 10.7 Braces with other constructs

- A branch may hold a `**`: `{src/**/*.ts,lib/**/*.js}`. The two globstars are independent (§9.6), and each is judged on its expansion (§9.1).
- `{[ab],c}` is legal. Brace and class scopes are independent (§4.4).
- Dot protection is judged per expansion (§12).

---

## 11. Negation

### 11.1 Parity

A run of `!` at the start of a pattern (§4.3) negates the whole pattern: `!P` matches exactly the paths `P` does not. An odd run negates; an even run cancels (`!!foo` ≡ `foo`, `!!!foo` ≡ `!foo`).

```
L(!P) = Σ* \ L(P)
```

### 11.2 Whole-pattern only

There is no negation of a segment, and the run is read before braces (§10.1). A `!` outside the leading run is literal: `!(foo)` is one negation of the literal body `(foo)` and matches every path but `(foo)`; `{!a,b}` holds no negation.

### 11.3 A literal leading `!`

A name that starts with `!` is matched with an escape: `\!foo` matches `!foo`, and so does `[\!]foo`. `[!]foo` does not: by §8.5 its `]` is a member and the class is unterminated.

### 11.4 Options

The leading `!` is syntax, not an option; no option disables it. §15.4 and §16.1 state what a negated pattern means to `match_dir` and to a union.

---

## 12. Dot Protection

Under `dot = false` (§3.1), a position `i` of a path is *dot-protected* when `path[i] = '.'` and `i` is a segment start (§2.3). A segment that starts at a protected position matches only a pattern segment whose first construct takes the `.` by name:

- a literal `.`, or a positive class that lists `.` (a range counts: `[,-.]`), takes it;
- a `*` there matches nothing, not even the empty string: `*.txt` does not match `.txt`, and `*.*` does not match `.a`;
- `?` and a negated class do not take the `.`;
- braces are judged per expansion (§10.1): `{*,.}a` matches `.a` through its expansion `.a`, and `*{.a,b}` does not;
- a `**` is itself unaffected, but every segment start inside it is subject to the rule: `**` matches neither `a/.b` nor `.a/b` (§9.2).

Under `dot = true` no position is protected: `*`, `?`, negated classes and `**` consume a leading `.` like any byte, in the names `.` and `..` included (§3.1).

---

## 13. Case Folding

Under `case_insensitive` (§3.2), ASCII letters compare case-folded wherever a pattern byte meets a path byte: `*.ts` matches `MAIN.TS`, and `[A-Z]` matches `a`.

Case folding is a rule of matching, not of syntax. Parsing MUST NOT depend on the option, so every validity check is byte-level and case-sensitive: `[a-A]` is `InvalidRange` under `case_insensitive = true` as well, and an error describes the pattern as written.

Folding covers ASCII only (§14.3). Non-ASCII bytes compare verbatim: `café.txt` matches `CAFé.txt` and not `CAFÉ.txt`. A caller who needs Unicode folding MUST normalize pattern and path first, for instance to NFKC and lower case.

Static prefixes are reported in the pattern's case (§17).

---

## 14. Matching a Path

### 14.1 Whole-path matching

`is_match(P, path) := path ∈ L(P)` (§2.4). A match covers the whole path. There is no substring matching, and no anchoring beyond that.

### 14.2 Separators on the path side

The matcher recognizes the set `Seps` (§2.2) wherever it asks whether a byte is a separator. Each pattern `/` consumes exactly one separator byte (§7.4); runs are accepted only at a globstar (§9.5). The matcher does not:

1. rewrite `\` to `/`: a separator byte is recognized, never substituted;
2. collapse `//` in a pattern: each `/` is a strict separator;
3. collapse `//` in a path: only the two lenient places of §9.5 absorb a run;
4. resolve `.` or `..`: they are matched as bytes;
5. resolve symbolic links: that is a walker's task.

### 14.3 Bytes, not characters

Matching is byte-wise (§2.1). `?` consumes one byte and not one code point: one three-byte character is matched by `???`. A class consumes one byte (§8.6), and case folding sees ASCII bytes only (§13).

### 14.4 Platforms

The same pattern and path MAY match on one platform and not on another, because `Seps` differs (§2.2). Where `\ ∈ Seps`, a path may use `\`, a literal `\` in a pattern (`\\`) matches nothing, and a class never matches `\` (§7.1, §8.2). A caller who needs portable results SHOULD normalize paths to `/` first.

---

## 15. Directory Matching

### 15.1 Definition

For a pattern `P` and a directory path `d`, `match_dir(P, d)` answers three questions and returns one of six results.

| Question                 | Name                | Meaning                                |
| ------------------------ | ------------------- | -------------------------------------- |
| does `d` match?          | `matches`           | `d ∈ L(P)`                             |
| may something below match? | `may_match_below` | some path below `d` may be in `L(P)`   |
| does everything below match? | `matches_all_below` | every path below `d` is in `L(P)` (§15.5) |

The third implies the second.

| Result               | `matches` | `may_match_below` | `matches_all_below` |
| -------------------- | --------- | ----------------- | ------------------- |
| `Match`              | true      | false             | false               |
| `Pruned`             | false     | false             | false               |
| `Descend`            | false     | true              | false               |
| `DescendAndMatch`    | true      | true              | false               |
| `DescendAll`         | false     | true              | true                |
| `DescendAllAndMatch` | true      | true              | true                |

The names read from the pattern's side. A consumer that selects files descends into `d` while `may_match_below` holds. A consumer that excludes files skips `d` whole once `matches_all_below` holds, and needs no test below `d` once `may_match_below` is false.

### 15.2 Semantics

```
below(d)         := Σ⁺                               if d = ""   (the walk root)
                    { d · "/" · w : w ∈ Σ* }         otherwise

matches(P, d)    := d ∈ L(P)
may_below(P, d)  := below(d) ∩ L(P) ≠ ∅
all_below(P, d)  := see §15.5
```

The two flags about "below" are one-sided. `may_match_below = false` is a guarantee that nothing below matches; `true` MAY be conservative. `matches_all_below = true` is a guarantee that everything below matches; `false` MAY be conservative. An implementation MUST NOT report `may_match_below = false` while `may_below(P, d)` holds, and MUST NOT report `matches_all_below = true` unless `all_below(P, d)` holds.

*Remark.* At the root (`d = ""`) the reference implementations report `may_match_below = true` for every pattern.

### 15.3 The query path

`d` is taken as given and never canonicalized. `match_dir("src", "src")` is `Match`. A trailing slash asks about that byte string: `match_dir("a/**", "a/")` is `DescendAndMatch` under `dot = false` (`a/ ∈ L`, `a//x ∈ L`, `a//.x ∉ L`) and `DescendAllAndMatch` under `dot = true`, while `match_dir("a/b", "a/")` is `Pruned`, since nothing in `{a/b}` starts with `a//`.

Directory listings return names without a trailing separator, so the slashless form is the canonical query: `a`, not `a/`, and a drive as `C:`, not `C:/`. The filesystem root has no slashless form, and `/` asks about the paths below `//`, so `match_dir("/p/**", "/")` is `Pruned`; the root is asked about as `""`, whose answers hold for every path.

The forms of `a` against `a/**`, under `dot = false`:

| Directory | `match_dir("a/**", d)` | Reason                                                      |
| --------- | ---------------------- | ----------------------------------------------------------- |
| `a`       | `Descend`              | `a` does not match; `a/x` may                               |
| `a/`      | `DescendAndMatch`      | `a/` matches; `a//x` matches through the lenient boundary (§9.5) |
| `a/b`     | `DescendAndMatch`      |                                                             |

Under `dot = true` every path below matches, and the three are `DescendAll`, `DescendAllAndMatch` and `DescendAllAndMatch` (§15.5).

### 15.4 Negated patterns

A negated pattern returns `DescendAndMatch` when `d` itself matches and `Descend` otherwise, never a covering result: `matches` is exact (§15.2), and the flags about "below" say only that something may match. In a union (§16.2) `matches` is the union's own answer for `d`, and a negated member contributes `may_match_below`.

*Remark.* `!P` usually means "everything except `P`", and a consumer must visit the whole tree to find it.

### 15.5 All-below

#### Definition

```
all_below(P, d) := ∀ s ∈ R.  (d = "" ? s : d · "/" · s) ∈ L(P)
R               := seg ( "/" seg )*        ; one or more segments
seg             := one or more non-separator bytes
```

Whether `d` itself matches plays no part.

*Remark.* A pattern that excludes files says nothing about a directory by matching it: `**/*.log` matches a directory named `foo.log` and not the file `foo.log/a.js` inside it. A consumer that skips a whole directory, such as a watcher applying an exclude list, needs to know that nothing below it escapes the pattern. That is what this flag states.

#### Guarantee

An implementation MUST NOT set `matches_all_below` unless `all_below(P, d)` holds. A consumer that skips a directory only on `matches_all_below` therefore never hides a path the pattern does not match. An implementation MAY leave the flag unset where it cannot prove the property within a fixed amount of work.

#### Precision

Under `dot = false` the flag is never set: `**` does not match `d/.cache`, so no subtree is covered.

Under `dot = true` the reference implementations set the flag when, from every state the directory can leave the pattern in, every name of one or more segments is accepted through wildcards alone: a `**` takes any segment, and a construct inside a segment counts when its wildcards alone take every non-empty name (`*`, `?*`, `{?,??*}`; not `?`, `*.js` or a class). Constructs are judged one at a time and not combined, so two shapes that cover a subtree are not proven and receive `Descend`:

- coverage through classes: `/p/dist/**/{a*,[!a]*}` takes every name, as one that starts with `a` or one that does not;
- coverage by length across branches: `?/**/?` with `**/??*` takes every name below `c/x.js`, as a one-byte name or a longer one; the brace `{**/?,**/??*}` likewise.

A union whose members are merged into one construct is proven: `["**/?", "**/??*"]` becomes `**/?{,?*}` and receives `DescendAllAndMatch` at `a`, while the brace `{**/?,**/??*}`, which keeps its two branches, receives `DescendAndMatch` there.

#### Examples

All rows use `dot = true` unless marked.

| Pattern                      | Directory             | Result               | A path below that escapes |
| ---------------------------- | --------------------- | -------------------- | ------------------------- |
| `/p/dist/**`                 | `/p/dist`             | `DescendAll`         | none                      |
| `**/node_modules/**`         | `/p/node_modules`     | `DescendAll`         | none                      |
| `**/node_modules/**`         | `/p/node_modules/pkg` | `DescendAllAndMatch` | none                      |
| `**`                         | `/p/a`                | `DescendAllAndMatch` | none                      |
| `/p/dist/**/*`               | `/p/dist`             | `DescendAll`         | none                      |
| `/p/dist/**/{a,*}`           | `/p/dist`             | `DescendAll`         | none                      |
| `{/p/dist/*,/p/dist/*/**}`   | `/p/dist`             | `DescendAll`         | none                      |
| `/p/dist/**/*.js`            | `/p/dist`             | `Descend`            | `/p/dist/a`               |
| `/p/dist/*/**`               | `/p/dist`             | `Descend`            | `/p/dist/a`               |
| `/p/dist/*`                  | `/p/dist`             | `Descend`            | `/p/dist/a/b`             |
| `/p/dist/**/?`               | `/p/dist`             | `Descend`            | `/p/dist/ab`              |
| `**/*.log`                   | `/p/foo.log`          | `DescendAndMatch`    | `/p/foo.log/a`            |
| `/p/dist/**` (`dot = false`) | `/p/dist`             | `Descend`            | `/p/dist/.cache`          |

### 15.6 Examples

| Pattern             | Directory                        | Result            | Reason                                     |
| ------------------- | -------------------------------- | ----------------- | ------------------------------------------ |
| `src/**/*.ts`       | `src`                            | `Descend`         | a `.ts` may lie below                      |
| `src/**/*.ts`       | `src/components`                 | `Descend`         |                                            |
| `src/**/*.ts`       | `tests`                          | `Pruned`          | not under `src`                            |
| `src/**/*.ts`       | `node_modules`                   | `Pruned`          | not under `src`                            |
| `src/**/*.ts`       | `src/main.ts` (as a directory)   | `DescendAndMatch` | matches; `src/main.ts/x.ts` would match    |
| `src/components`    | `src`                            | `Descend`         |                                            |
| `src/components`    | `src/components`                 | `Match`           | exact; nothing continues below             |
| `src/components/**` | `src/components`                 | `Descend`         | `**` requires the `/` before it            |
| `src/components/**` | `src/components/a`               | `DescendAndMatch` |                                            |
| `{src,lib}/**`      | `src`                            | `Descend`         |                                            |
| `{src,lib}/**`      | `docs`                           | `Pruned`          |                                            |
| `**/*.ts`           | `any/dir`                        | `Descend`         | `**` allows any depth                      |

These are the results under `dot = false`. Under `dot = true` the rows whose pattern ends in `/**` and whose directory lies at or below that `/**` report the covering results: `src/components/**` gives `DescendAll` on `src/components` and `DescendAllAndMatch` on `src/components/a`, and `{src,lib}/**` gives `DescendAll` on `src` (§15.5).

---

## 16. Unions

### 16.1 Definition

A union is the disjunction of `N ≥ 1` patterns, compiled into one matcher:

```
L(union) = ⋃ᵢ L(Pᵢ)
```

A union answers the same questions as a pattern (`is_match`, `match_dir`, `static_prefixes`); which member matched is not part of the answer. A union of no patterns is the error `EmptyPatternSet` (§6.1). A pattern given twice counts once. A list of members that each compile compiles: the members share no limit (§6.2). An error one member raises identifies the member (§6.3).

An implementation MAY reject a negated member with `NegatedInUnion` or MAY accept it; the reference implementations differ (Rust rejects, JS accepts). Where accepted, a negated member contributes its complement to the disjunction: `["src/**", "!**/*.test.ts"]` matches `lib/a.ts`, because the second member does. A `!` member never subtracts from the others; include-and-exclude filtering belongs to a walker (WALKER_SPEC.md §4).

### 16.2 Directory matching of a union

```
matches(union, d)    := ∃ i. matches(Pᵢ, d)
may_below(union, d)  := ∃ i. may_below(Pᵢ, d)
all_below(union, d)  := all_below over L(union)        (§15.5)
```

A union MUST NOT report `Pruned` unless every member does. A negated member, where accepted, contributes its own `matches` and `may_below` (§15.4). All-below is a property of the union's language: one covering member suffices, and members that each cover a part cover together (`a/*` the children, `a/*/**` everything deeper).

---

## 17. Static Prefixes

`static_prefixes(P)` is a finite set of byte strings such that every path in `L(P)` starts with one of them, where "starts with" means byte for byte, with `/` as the separator, and either the whole path or a prefix followed by a separator. A walker seeds its traversal from them (WALKER_SPEC.md §6).

An implementation MUST return a sound set: no path in `L(P)` may lack a prefix in it. The empty string is always sound and covers every other prefix. The reference implementations return, for each brace expansion of `P` (§10.1), its literal head up to its last whole segment, with heads a shorter one covers removed:

| Pattern             | Static prefixes                  |
| ------------------- | -------------------------------- |
| `src/main.rs`       | `{ "src/main.rs" }`              |
| `src/*.ts`          | `{ "src" }`                      |
| `src/**/*.ts`       | `{ "src" }`                      |
| `**/*.ts`           | `{ "" }`                         |
| `{src,test}/*.rs`   | `{ "src", "test" }`              |
| `{src,src/a}/*.rs`  | `{ "src" }`                      |
| `{src,lib}/main.rs` | `{ "lib/main.rs", "src/main.rs" }` |

An expansion with no literal head gives the empty prefix, and so does a negated pattern or member, whose matches may lie anywhere. The prefixes are in the pattern's case (a case-insensitive match may differ in case, §13) and hold `/` only (a path may hold other separators, §14.4). An expansion that can match nothing contributes no prefix (on Windows, one through an escaped `\`, §4.2), so a pattern whose expansions are all dead has an empty set. A head MAY be cut back to an earlier segment boundary: a shorter prefix is still sound.

---

## Appendix A: Differences from Other Tools

| Behavior                                      | globstar                            | Bash               | picomatch                       | globset                      | fast-glob (npm)               | fast-glob (crate)              | doublestar                                     |
| --------------------------------------------- | ----------------------------------- | ------------------ | ------------------------------- | ---------------------------- | ----------------------------- | ------------------------------ | ---------------------------------------------- |
| `dot` default                                 | `true` (matcher) / `false` (walker) | `false`            | `false`                         | `true` (no concept)          | `false`                       | `true` (no concept)            | `true` (`Glob`: `WithNoHidden()`)              |
| Case-insensitive                              | ASCII only, default `false`         | `shopt nocaseglob` | `nocase` option                 | `case_insensitive` builder   | `caseSensitiveMatch: false`   | ❌                              | `WithCaseInsensitive()` (`Glob` only), Unicode |
| `**` must own a segment                       | enforced                            | ✅                  | ✅                               | ✅ (with `literal_separator`) | ✅                             | ✅                              | ✅                                              |
| `a/**` matches `a`                            | ❌                                   | ⚠️ (lists `a/`)    | ✅ (❌ with `strictSlashes`)      | ❌                            | ⚠️ (walker: no; matcher: yes) | ❌                              | ✅                                              |
| `**` in braces judged on the expansion (§10.1) | ✅                                   | ✅ (expands first)  | ❌ (branch-local `.*`)           | ❌                            | ✅ (expands first)             | ❌                              | ⚠️ (not `**{/a,b}`)                            |
| `**` in a single-branch brace `{**}` (§10.4)  | a star                              | a star             | keeps globstar                  | a group; `**` acts as `*`    | keeps globstar                | a group (= `**`)               | a group (= `**`)                               |
| `**/X` matches the absolute `/a/X` (§9.4)     | ✅                                   | n/a                | ✅                               | ✅                            | n/a (walker)                  | ✅                              | ✅                                              |
| Extglobs                                      | ❌ (literal)                         | ✅ (`shopt`)        | ✅                               | ❌                            | ✅                             | ❌                              | ❌                                              |
| POSIX character classes                       | ❌                                   | ✅                  | ✅                               | ❌                            | ✅                             | ❌                              | ❌                                              |
| `[/]` inside a class (§8.2)                   | parse error                         | literal            | ⚠️ (`/` or the literal text)    | matches `/`                  | never matches                 | never matches                  | ⚠️ (`Match`: `/`; `Glob`: error)               |
| First `]` is a member (§8.5)                  | ✅                                   | ✅                  | ✅                               | ✅                            | ✅                             | ✅                              | ❌ (error)                                      |
| `[!a]` same as `[^a]`                         | ✅ (both negate)                     | ✅                  | ❌ (only `[^a]`; ✅ with `posix`) | ✅                            | ✅                             | ✅                              | ✅                                              |
| Brace ranges `{1..10}`                        | ❌                                   | ✅                  | ✅                               | ❌                            | ✅                             | ❌                              | ❌                                              |
| Brace nesting                                 | ≤ 32                                | ✅                  | ✅                               | ✅                            | ✅                             | ≤ 10                           | ✅                                              |
| Leading-`!` negation                          | ✅                                   | n/a                | ✅                               | ❌ (literal)                  | ✅ (an ignore pattern)         | ✅                              | ❌ (literal)                                    |
| `/` required in patterns                      | ✅                                   | n/a                | ✅                               | ⚠️                           | ✅                             | ✅                              | ⚠️ (`PathMatch`)                               |
| `\` is always the escape                      | ✅                                   | ✅                  | ✅                               | ⚠️                           | ✅                             | ✅ ¹                            | ⚠️ (`PathMatch`)                               |
| `\` in a path is a separator (§14.2)          | platform-conditional                | ❌                  | ⚠️ (`windows` option)           | platform                     | n/a (walker)                  | platform ¹                     | platform (`PathMatch`)                         |
| Stray `]`, `}`, `)`                           | literal                             | literal            | literal                         | `}` is an error              | literal                       | literal                        | ⚠️ (`}`: `Glob` error)                         |
| Unclosed constructs                           | parse error                         | literal            | ⚠️                              | error                        | ⚠️ (no error)                 | no match (`validate()` errors) | error                                          |
| `match_dir`                                   | ✅                                   | n/a                | ❌                               | ❌                            | ❌                             | ❌                              | ❌                                              |
| `*` and `**` match `.` and `..` (dot on)      | ✅                                   | ❌                  | ❌                               | ✅                            | ❌                             | ✅                              | ✅                                              |

Symbols: ✅ yes, ❌ no, ⚠️ partial or conditional, n/a not applicable. ¹ Measured on Unix only; the crate has no platform switch for `\` in patterns and splits paths with `std::path::is_separator`.

Every cell was measured on 2026-10-16: Bash 5.2.37 (filename expansion with `globstar`, and `[[ ]]` where no file can exist), picomatch 4.0.7 (4.0.4 and 2.3.2 answer the same), globset 0.4.20 (with `literal_separator(true)`), fast-glob 3.3.3 from npm (a walker; it matches through micromatch 4.0.8 and picomatch 2.3.2 and expands braces first), the fast-glob crate 1.1.2 (`glob_match`) and doublestar v4.10.3 (`Match`, and `Glob` on real trees). Windows cells come from a simulated platform. Tools change between versions.

---

## Appendix B: Glossary

| Term                   | Meaning                                                                                                   |
| ---------------------- | --------------------------------------------------------------------------------------------------------- |
| Pattern                | The glob string, read as UTF-8 bytes (§2.1).                                                              |
| Path                   | The byte string being matched (§2.1).                                                                     |
| `Seps`                 | The set of separator bytes in a path (§2.2). `/` is always a member.                                      |
| Segment                | A run of non-separator bytes between separators; a path is segments joined by single separators (§2.3).   |
| Segment start          | Position 0, or a position right after a separator (§2.3).                                                 |
| `L(P)`                 | The set of paths a pattern denotes under the options in force (§2.4).                                     |
| Globstar               | A `**` that owns a whole segment (§9.1). Any other `**` is a star.                                        |
| Expansion              | One pattern obtained by choosing a branch in every brace (§10.1); a pattern denotes the union of its expansions. |
| Dot-protected position | A `.` at a segment start, under `dot = false` (§12). No wildcard consumes it.                             |
| Wildcard               | `*`, `?`, `**` and a negated class: the constructs dot protection restricts (§12).                        |
| `match_dir` results    | `Match`, `Pruned`, `Descend`, `DescendAndMatch`, `DescendAll`, `DescendAllAndMatch` (§15).                |
| All-below              | Every path below a directory matches (§15.5). The two `DescendAll…` results carry it.                     |
| Union                  | The disjunction of several patterns, compiled into one matcher (§16).                                     |
| Static prefixes        | Literal heads every match starts with, at a segment boundary (§17). Walkers seed from them.               |
