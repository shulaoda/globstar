# GLOB_SPEC: Syntax and Semantics

**Status.** This document is the normative definition of the glob dialect implemented by the `globstar` Rust crate (`crates/globstar`) and the `@globstar/core` JS package (`packages/globstar`). When code, tests or other documents disagree with it, the code changes.

**Version.** 0.2.4, draft. The document is not frozen; Appendix A (§17) lists every change.

**Baseline.** The dialect follows `picomatch` and `fast-glob` mainline behavior with the rigor of `doublestar`. Bit-exact parity with any single tool is not a goal; §16 lists every deliberate difference.

**Conventions.** The key words MUST, MUST NOT, SHOULD and MAY are to be read as described in RFC 2119. They state requirements on an implementation or on a caller. Sentences without a key word define the pattern language itself. A paragraph marked *Note*, *Rationale* or *Implementation note* is informative. Examples are informative; the executable corpus (§15) is the test of conformance.

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

- **Pattern constructs:** `*`, `**`, `?`, character classes `[...]`, braces `{...}`, the escape `\`, and leading-`!` negation.
- **Match semantics:** what `is_match(pattern, path)` returns for every pair of inputs.
- **Directory matching:** the `match_dir` operation and its six results (§13). Walkers and watchers consume it; this document defines only what each result means.
- **Multi-pattern matchers:** the boolean OR of several patterns (§14). In Rust this is `Glob::union`; in JS, `globstar(patterns)` and `compileMatcher(patterns)`.

Filesystem traversal is the subject of [WALKER_SPEC.md](./WALKER_SPEC.md): cwd handling, the auto-split of `!` patterns into an ignore set, traversal order and walker errors.

### 1.2 Out of scope

- **Bash extglobs** `@()`, `?()`, `*()`, `+()`, `!(...)`: the parentheses are literal bytes (§5).
- **POSIX character classes** `[[:alpha:]]`, collating elements `[[.a.]]`, equivalence classes `[[=a=]]` (§6.6).
- **Bash brace ranges** `{1..10}`, `{a..z}`.
- **Regex features:** backreferences, groups, lookaround.
- **Gitignore re-include** ("a later rule cancels an earlier one"): a walker-layer concern.
- **Unicode case folding:** only ASCII folding is offered (§11.2).
- **Shell expansions:** `~`, `$VAR`.

### 1.3 Compatibility statement

The closest reference is `picomatch`. Every deliberate divergence is listed in §16.

---

## 2. Character Set and Input Conventions

### 2.1 Character set

Matching is defined over bytes.

- **Patterns** are Unicode strings (`&str` in Rust, JS strings), read as their UTF-8 bytes. Non-UTF-8 pattern bytes are not accepted.
- **Paths** are arbitrary bytes in Rust (`is_match(&[u8])`), so non-UTF-8 file names match byte for byte. In JS a path is a string, encoded to UTF-8 internally; a lone surrogate encodes as U+FFFD. A non-string JS input throws a `TypeError`.
- Only ASCII bytes can carry a meta meaning. Every byte outside ASCII is literal.
- Code point boundaries play no part: comparison is byte for byte (§12.5).

### 2.2 Path separator in patterns

Inside a pattern, `/` is the only path separator, on every platform. `\` is the escape character (§2.4), never a separator.

| Pattern        | Separator? | Meaning                                        |
| -------------- | ---------- | ---------------------------------------------- |
| `src/main.rs`  | yes        | two segments                                   |
| `src\main.rs`  | no         | one segment, `srcmain.rs`; the same on Windows |
| `src\\main.rs` | no         | one segment, `src\main.rs` with a literal `\`  |

A tool that accepts native-shell paths as patterns MUST normalize them to `/` first.

### 2.3 Path separator in matched paths

A path MAY use `/` or the platform's native separator. The matcher recognizes an implementation-defined separator byte set `Seps`:

- `/` ∈ `Seps` on every platform; this document mandates it.
- Other bytes (typically `\`) are the implementation's choice and SHOULD follow platform norms.
- The reference implementations define `Seps := { b : std::path::is_separator(b as char) }`: `\` is a separator on Windows and nowhere else.

The consequences are detailed in §12.3.

### 2.4 Escape character

`\` is always the escape character inside a pattern. Its meaning depends on the byte `X` that follows it:

| Next byte          | Result                                                                      |
| ------------------ | --------------------------------------------------------------------------- |
| `/`                | parse error `EscapedSeparator`                                              |
| end of the pattern | parse error `TrailingBackslash`                                             |
| any other `X`      | the literal byte `X`, whether `X` is a meta byte (`\*`) or not (`\a` ≡ `a`) |

> **Rationale.** No file name on any platform contains a `/`, so an escaped separator could never match; reporting it is more useful than compiling a pattern that matches nothing.

`\\` yields a literal `\`. Like a class member (§6.2) it never matches a separator, so on Windows, where `\` ∈ `Seps`, an escaped backslash never matches: a platform-dependent silent miss.

---

## 3. BNF Grammar

```bnf
Pattern      ::= [Negation] Sequence
Negation     ::= "!"+              ; leading run; its parity decides (§9.3)
Sequence     ::= Atom*
Atom         ::= Literal | Separator | AnyChar | Star | Globstar
              |  CharClass | Brace | Escaped

Literal      ::= a byte with no meta role at this position
Separator    ::= "/"
AnyChar      ::= "?"
Star         ::= "*"               ; when not part of a Globstar
Globstar     ::= "**"              ; only where it owns a path segment (§8.1)

CharClass    ::= "[" ("^" | "!")? CharClassItem+ "]"
CharClassItem::= Literal | Range | Escaped
Range        ::= Literal "-" Literal

Brace        ::= "{" Sequence ("," Sequence)+ "}"
                                   ; a `{...}` with no comma is literal (§7.4)
                                   ; branches may be empty (§7.3)
Escaped      ::= "\" AnyByte       ; \X yields the literal byte X (§2.4)
```

The meta bytes are `* ? [ ] { } , \`, plus `!` at the very start of the pattern.

- `,` separates branches only inside a brace; elsewhere it is a literal byte.
- `]` and `}` are meta only while the matching `[` or `{` is open; otherwise they are literal bytes (§9.1).
- `!` negates only as a leading run (§9.3). Anywhere else it is a literal byte.
- `|`, `@`, `+`, `(` and `)` are always literal bytes.

---

## 4. Semantics of Basic Constructs

### 4.1 `Literal`

Matches exactly that byte, once.

### 4.2 `?`

Matches exactly one byte that is not a separator (not in `Seps`, §2.3). It does not match the empty string. Under `dot = false`, a `?` at a segment start does not match `.` (§12.4).

### 4.3 `*`

Matches zero or more non-separator bytes.

- It matches the empty string: `foo*bar` matches `foobar`.
- It does not cross `/`: `src/*` does not match `src/a/b`.
- Under `dot = false`, a `*` at a segment start does not consume `.`: `*.txt` does not match `.txt` (§12.4).

### 4.4 `**`

Specified in §8.

### 4.5 Empty pattern `""`

Parse error `Empty`.

> **Rationale.** The empty pattern has no obvious meaning, so it is rejected rather than given one.

---

## 5. Extglobs (out of scope)

Bash extglobs `@()`, `?()`, `*()`, `+()`, `!(...)` are not supported. `(`, `)`, `@`, `+` and `|` are literal bytes; a `?` or `*` inside keeps its wildcard meaning. `@(a|b)` therefore matches only the path `@(a|b)`.

For alternation use a brace `{a,b}`. For negation use the leading `!` (§9.3).

---

## 6. Character Class Semantics

### 6.1 Basic forms

| Form       | Kind    | Matches                                |
| ---------- | ------- | -------------------------------------- |
| `[abc]`    | set     | `a`, `b` or `c`                        |
| `[a-z]`    | range   | any byte in `a..=z`                    |
| `[abc0-9]` | mixed   | a set member or a range member         |
| `[!abc]`   | negated | any byte that is none of `a`, `b`, `c` |
| `[^abc]`   | negated | the same as `[!abc]`                   |

### 6.2 Character classes are segment-local

A class closes inside the segment that contains it. A `/` in the class body, written directly (`[/]`, `[a/b]`, `[^/]`) or escaped (`[\/]`), ends the segment before a closer was found, and the parser MUST report `UnterminatedClass`.

At match time a class never matches a member of `Seps`, whatever its polarity. `/` is always in `Seps`, so this rule only adds something on platforms where `\` is a separator (Windows, §12.3): there `[^abc]` rejects `\`, and the positive class `[\\]` never matches. On Unix `\` is an ordinary byte and `[\\]` matches it.

> **Rationale.** Compiling `foo[/]bar` to a class that can never match would hide the mistake; an error reports it at once.

### 6.3 Special bytes inside a class

| Byte                                   | Role            | When                                                     |
| -------------------------------------- | --------------- | -------------------------------------------------------- |
| `]`                                    | closer          | not in first position (first position: §6.5)             |
| `-`                                    | range separator | between two member bytes; literal next to `[` or `]`     |
| `!`, `^`                               | negation marker | immediately after `[`                                    |
| `\`                                    | escape prefix   | always; the next byte becomes a literal member           |
| `/`                                    | illegal         | any position: `UnterminatedClass` (§6.2)                 |
| `*`, `?`, `{`, `}`, `(`, `)`, `@`, `+` | literal         | meta bytes from outside a class are plain members inside |
| any other byte                         | literal         |                                                          |

Escapes inside a class follow §2.4: `\X` is the member `X`, whatever `X` is. Thus `[\?]` holds `?`, `[\-]` holds `-` (no range), `[\!]` is a positive class holding `!`, and `[\]]` holds `]` because the escape consumes the first `]` and the second closes.

| Pattern            | Result                  | Why                                                  |
| ------------------ | ----------------------- | ---------------------------------------------------- |
| `[?*!(]`           | class `{?, *, !, (}`    | meta bytes are plain members inside                  |
| `[a-z]`            | range `a..=z`           | `-` between two members                              |
| `[a\-z]`           | class `{a, -, z}`       | the escaped `-` is a member, not a range             |
| `[-abc]`, `[abc-]` | class with `-`          | `-` next to a bracket is literal                     |
| `[!abc]`           | negated `{a, b, c}`     | `!` in first position                                |
| `[\!abc]`          | positive `{!, a, b, c}` | the escape overrides the negation                    |
| `[]]`              | class `{]}`             | first-`]` rule (§6.5)                                |
| `[!]]`             | negated `{]}`           | first-`]` after the negation marker                  |
| `[\]]`             | class `{]}`             | escaped form                                         |
| `[\]`              | `UnterminatedClass`     | the escape consumes the only `]`                     |
| `[]`               | `UnterminatedClass`     | first `]` is a member; no closer follows             |
| `[/]`, `[\/]`      | `UnterminatedClass`     | separator in a class (§6.2)                          |
| `[\\]`             | class `{\}`             | matches `\` on Unix; never matches on Windows (§6.2) |

### 6.4 Negated classes and the `dot` option

Under `dot = false`, a negated class at a segment start does not match `.`; under `dot = true` it does. This keeps dot protection consistent across every wildcard construct (§12.4). A positive class with `.` as a member (`[.]`, `[.a]`) matches `.` under either setting.

### 6.5 Empty classes and the POSIX first-`]` rule

A `]` immediately after `[`, `[!` or `[^` is a member, not the closer, as in bash, fnmatch, picomatch and fast-glob. Consequences:

- `[]]` matches `]`; `[!]]` matches any byte except `]`; `[]-]` matches `]` or `-`.
- `[]`, `[!]` and `[^]` are `UnterminatedClass`: the `]` was a member, and no closer follows.
- `[` alone is `UnterminatedClass`.
- `[a-]` is the class `{a, -}`: a trailing `-` is a member (§10, lenient cases).

### 6.6 POSIX character classes

Not supported, and not recognized. `[[:alpha:]]` parses as the class `[[:alpha:]` (members `[`, `:`, `a`, `l`, `p`, `h`) followed by a literal `]`. It matches `a]` and never a letter on its own.

---

## 7. Brace Expansion Semantics

### 7.0 Normative definition: the expansion equation

A brace is pure union. For any fragments `pre`, `post` and branches `A`, `B`, …:

```
L( pre{A,B,…}post ) = L( preApost ) ∪ L( preBpost ) ∪ …
```

Each expansion is read as a standalone pattern under every rule of this document. In particular, `**` segment ownership (§8.1) is judged on the expanded form. A brace neither grants a `**` powers it lacks outside (`a{**,x}b` ≡ `a*b` ∪ `axb`, because the expanded neighbors are `a` and `b`) nor takes away powers it has (`{**,x}/b` ≡ `**/b` ∪ `x/b`, which matches `b`). The same holds for the lenient `/**/` boundary (§12.3) and the leading `**/` (§8.5): `x{/,y}{**,a}/z` contains `x/**/z` and matches `x//z`.

Two exceptions keep the equation from over-applying:

1. The leading `!` is read before expansion (§9.3). `{!a,b}` expands to the body `!a`, whose `!` is a literal byte.
2. An empty expansion means ε, not an error. The empty branch of `{a,}` contributes the empty string; the `Empty` error (§10) applies to the user's pattern only.

The equation works on tokens, not bytes. A branch is spliced in as a unit and tokens never fuse across the splice: `*{,a}*` is `L(* ε *)` ∪ `L(* a *)`, and the two stars of the empty expansion stay two stars (which collapse to one, §10). They do not become a `**` the author never wrote.

> **Implementation note.** The equation defines meaning only; §7.2 still forbids pre-expanding braces, and an implementation need only be equivalent. The reference implementations decide a `**` beside plain tokens while parsing. A `**` whose neighbor is a brace edge that can be a separator, or empty, is handled by distributing that brace over it (`**{/a,b}` → `{**/a,**b}`, `{a/,b}**` → `{a/**,b**}`); two braces meeting at a `**` edge (`{**,a}{/,b}`) merge into one. Only the branches the `**` can affect are taken apart, and branches that put a `/` toward the `**` share that one `/` (`**{/a,/b}` → `**/{a,b}`). This local expansion is what §7.6 and §7.7 bound.

### 7.1 Basic forms

| Form         | Matches                                   |
| ------------ | ----------------------------------------- |
| `{a,b,c}`    | `a`, `b` or `c`                           |
| `{a*b,c?}`   | a branch may hold wildcards               |
| `{a,{b,c}}`  | nested braces: `a`, `b` or `c`            |
| `{a,b}{c,d}` | Cartesian product: `ac`, `ad`, `bc`, `bd` |

### 7.2 No pre-expansion

An implementation MUST NOT expand braces into N separate patterns at parse time; braces are built into the matcher. Memory and match cost therefore do not blow up on `{a,b}{c,d}{e,f}…`. Braces whose branches cross a `/` are the exception: they can make the segments of a pattern multiply, and compilation may then fail with `TooComplex` (§10).

### 7.3 Empty branches

`{a,}` is legal and means `a` or the empty string.

### 7.4 Single-branch braces

`{a}` (no comma) is not a brace. It is the literal three bytes `{a}`, as in bash, picomatch and fast-glob.

The now-literal `{` or `}` is a plain byte, not a segment boundary, so a `**` next to it degrades to `*` (§8.1): `{**}` ≡ `{*}` matches `{a}` but not `{a/b}`, and `{a/**}` ≡ `{a/*}`. A `**` with `/` on both sides stays a globstar: `{a/**/b}` matches `{a/x/y/b}`. This is a deliberate divergence from picomatch and fast-glob, which keep the globstar (§16).

A separator stays structural; it never becomes literal content. `x{a/b}y` is the two-segment pattern `x{a` / `b}y`, exactly like `x\{a/b\}y`.

> **Rationale.** Brace expansion needs alternatives: `bash -c 'echo {a}'` prints `{a}`, while `echo {a,b}` prints `a b`. To match a file name with braces, write them bare (no comma, no brace) or escape them (`\{a\}`). Classes differ: `[a]` is still a class, because bracket expressions never had a minimum-member rule.

### 7.5 Unclosed braces

`{a,b` is a parse error, `UnterminatedBrace`.

### 7.6 Nesting depth limit

An implementation MUST accept braces nested up to 32 levels deep and MUST reject deeper nesting with `BraceNestingTooDeep` (§10). The depth counts every `{` as it opens, including one that later turns out to be literal (§7.4). The limit also bounds the local expansion around `**` (§7.0), which can nest braces the pattern wrote side by side: `**` followed by `{,/a}` 33 times fails the same way.

### 7.7 Expansion cap

Branch counts and Cartesian breadth are not bounded, because nothing is pre-expanded (§7.2); only the work of compiling is (`TooComplex`, §10).

The one bounded thing is the local expansion around `**` (§7.0). Every copy it makes is counted: one per token, a literal or class by its length, and one per brace branch. When the count would pass 4096, compilation MUST fail with `BraceExpansionTooLarge` (§10). The count runs over the whole pattern, and over all patterns of one union together (§14.1).

Two shapes reach the cap: a chain of braces that each put a `**` beside the next, since each link copies what the chain built so far (`{**,a}{/,b}` repeated five times); and a `**`-edged brace beside a long list of mixed branches, which is copied once. Branches that all start with `/` are not copied (`{**,dist}{/a,/b,…}`, `**{/a,/b,…}`), and neither is a brace that only shares a `/` with the next (`{**,a}/` repeated any number of times).

### 7.8 Braces and `**`

A branch may contain `**`: `{src/**/*.ts,lib/**/*.js}`. The two `**` are independent.

### 7.9 Braces and character classes

`{[ab],c}` is legal. Brace and class scopes are independent (§9.2).

---

## 8. Path Separator and `**` Semantics

### 8.1 Globstar must own a segment

`**` is a globstar if and only if it is preceded by `/` or the start of the pattern, and followed by `/` or the end of the pattern. Otherwise it is two `*`, which collapse to one.

| Pattern       | `**` is a globstar? | Reads as |
| ------------- | ------------------- | -------- |
| `src/**/*.ts` | yes                 |          |
| `**/*.ts`     | yes                 |          |
| `**`          | yes                 |          |
| `src/**`      | yes                 |          |
| `a**b`        | no                  | `a*b`    |
| `a/**b`       | no                  | `a/*b`   |
| `a**/b`       | no                  | `a*/b`   |

Next to a brace the test uses the expanded form (§7.0), one expansion at a time. The neighbor of a `**` beside a brace is the first or last token of each branch, or whatever lies beyond the brace for an empty branch; the neighbor of a `**` at a branch edge is whatever surrounds the brace. The same `**` can be a globstar in some expansions and a star in others.

| Pattern      | Expansions       | Note                                                        |
| ------------ | ---------------- | ----------------------------------------------------------- |
| `a{**,x}b`   | `a*b` ∪ `axb`    | degrades                                                    |
| `{**,x}/b`   | `**/b` ∪ `x/b`   | matches `b`                                                 |
| `{a/**,x}c`  | `a/*c` ∪ `xc`    | degrades                                                    |
| `a/{**/x,y}` | `a/**/x` ∪ `a/y` | a globstar with the lenient `/**/` boundary; matches `a//x` |
| `**{/a,b}`   | `**/a` ∪ `*b`    |                                                             |
| `{a/,b}**`   | `a/**` ∪ `b*`    |                                                             |
| `{a,}**`     | `a*` ∪ `**`      | the empty branch leaves the `**` alone                      |

### 8.2 Match semantics

A globstar matches any byte sequence, separators included, the empty sequence included. With Σ the set of all bytes:

```
L(**)      = Σ*
L(a/**)    = { "a/" + w : w ∈ Σ* }                        ; includes "a/"
L(**/b)    = { w + "b" : w ∈ Σ*, w empty or ending in / }
L(a/**/b)  = { "a/" + w + "b" : w empty or ending in / }  ; a/b, a/x/b, a/x/y/b, …
```

`**` is the only construct allowed to cross `/`; that is its sole difference from `*`.

### 8.3 `a/**` against the forms of `a`

| Path    | `is_match` | Reason                     |
| ------- | ---------- | -------------------------- |
| `a`     | false      | the prefix `a/` is missing |
| `a/`    | true       | `a/` plus the empty suffix |
| `a/b`   | true       |                            |
| `a/b/c` | true       |                            |

This agrees with picomatch, fast-glob and globset, and deliberately differs from doublestar, which makes `a/**` match `a`. To select a directory and everything below it, write `{a,a/**}`.

### 8.4 `match_dir` and `a/**`

Walkers query directories without a trailing `/`. Results under `dot = false`:

| Directory | `match_dir(d, "a/**")` | Reason                                                     |
| --------- | ---------------------- | ---------------------------------------------------------- |
| `a`       | Descend                | `a` does not match; `a/x` may                              |
| `a/`      | DescendAndMatch        | `a/` matches; `a//x` matches too (lenient boundary, §12.3) |
| `a/b`     | DescendAndMatch        |                                                            |

Under `dot = true` every path below `a` matches, so the three become `DescendAll`, `DescendAllAndMatch` and `DescendAllAndMatch` (§13.6).

### 8.5 Leading `**/` ("at any depth")

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

With braces this follows from §7.0. In `{**/a,README}` only the `**/a` expansion begins with `**/`, so the pattern matches `/a` but not `/README`: a literal branch stays strict. A `**/` in the middle of a pattern (`a/{**/x,y}`) is not leading; it keeps the lenient `/**/` boundary of §12.3 and nothing more.

### 8.6 Consecutive `**/**/`

Normalized to a single `**/`; the language is unchanged.

### 8.7 `**` does not cross brace boundaries

In `{src/**,lib/**}` the two `**` are independent; neither is merged with or aliased to the other.

### 8.8 `**` and `match_dir`

Specified in §13. In short, a pattern that holds a `**` returns `Descend` for almost every directory, and pruning comes from the structure around the `**`.

---

## 9. Escapes and the Leading `!`

### 9.1 Escape `\X` and the meta bytes

The escape rule is §2.4: `\X` is the literal byte `X`, except `\/` (`EscapedSeparator`) and a trailing `\` (`TrailingBackslash`). `\*` is `*`, `\\` is `\`, `\a` ≡ `a`.

The meta bytes fall in two groups:

| Group        | Bytes     | Bare form                                                                                 | Literal form           |
| ------------ | --------- | ----------------------------------------------------------------------------------------- | ---------------------- |
| Always meta  | `* ?`     | always a wildcard                                                                         | `\*`, `\?`             |
| Context meta | `[ ] { }` | an opener starts a scope that MUST be closed; a closer is meta only inside its open scope | `\[`, `\]`, `\{`, `\}` |

- `[` always opens a class and MUST be closed, else `UnterminatedClass`.
- `{` always opens a brace and MUST be closed, else `UnterminatedBrace`.
- `]` and `}` with no open scope are literal: `a]b` matches `a]b`; `{a,}}` is the brace `{a,}` then a literal `}`, matching `a}` and `}`.
- `(` and `)` have no role at all: `file(1).txt` and `@(a|b)` are literal.

All closers share one rule, meta inside their opener's scope and literal outside, and the MUST-close contract belongs to the openers.

### 9.2 Scanner independence

The parser is one left-to-right pass. Each opener hands control to a sub-scanner that consumes input by its own rules until it finds its own closer, unaware of the outer context. The first opener owns the input, so an inner closer can be invisible to the outer scanner.

| Pattern  | Trace                                                                          | Result              |
| -------- | ------------------------------------------------------------------------------ | ------------------- |
| `[{]}`   | class holds `{`, closes at `]`; trailing `}` is literal                        | matches `{}`        |
| `{[}]`   | brace opens; the class consumes `}` as a member and closes at `]`; no `}` left | `UnterminatedBrace` |
| `[}{]`   | class holds `}` and `{`                                                        | matches `}` or `{`  |
| `{a,[]}` | brace opens; the class takes `]` as its first member; no second `]` arrives    | `UnterminatedClass` |
| `[(])`   | class holds `(`; trailing `)` is literal                                       | matches `()`        |

> **Rationale.** This is the model of bash brace expansion, POSIX bracket expressions and regex. A pattern that is visually balanced but structurally crossed, such as `{[}]`, is almost always a typo; an error is more useful than bash's silent fallback to literal text.

### 9.3 Leading `!`

A run of `!` at the start of a pattern negates the whole pattern: `!P` matches exactly the paths `P` does not. An odd run negates, an even run cancels (`!!foo` ≡ `foo`, `!!!foo` ≡ `!foo`).

There is no segment-level negation. A `!` outside the leading run is a literal byte: `!(foo)` is one negation over the literal body `(foo)`, matching every path except `(foo)`.

### 9.4 Literal leading `!`

To match a name that starts with `!`, escape it: `\!foo` matches `!foo`. `[\!]foo` works too. `[!]foo` does not: by §6.5 its `]` is a member and the class is unterminated.

### 9.5 Counting the leading `!`

1. Scan the run of `!` bytes at position 0.
2. An odd count produces one negation; an even count produces none.
3. The `Sequence` starts at the first non-`!` byte; every later `!` is literal.

The run is read before braces expand (§7.0), so `{!a,b}` has no negation.

### 9.6 Interaction with options

The leading `!` is always honored. It is part of the pattern syntax, not a matching option, so no option disables it.

---

## 10. Compile-time Errors

The parser MUST reject the following inputs:

| Error                    | Example                    | Reason                                                      |
| ------------------------ | -------------------------- | ----------------------------------------------------------- |
| `Empty`                  | `""`                       | no defined meaning (§4.5)                                   |
| `TooLong`                | more than 65,536 bytes     | resource guard                                              |
| `UnterminatedClass`      | `[abc`, `[]`, `[/]`        | an opener needs its closer (§6.2, §6.5)                     |
| `UnterminatedBrace`      | `{a,b`                     | an opener needs its closer (§7.5)                           |
| `TrailingBackslash`      | `foo\`                     | nothing to escape (§2.4)                                    |
| `EscapedSeparator`       | `a\/b`                     | no file name holds a `/` (§2.4)                             |
| `BraceNestingTooDeep`    | 33 nested `{`              | resource guard, also after the expansion around `**` (§7.6) |
| `BraceExpansionTooLarge` | `{**,a}{/,b}` × 5          | resource guard for the expansion around `**` (§7.7)         |
| `TooComplex`             | `{a,b,/}` × 100            | resource guard for compile work, see below                  |
| `InvalidRange`           | `[z-a]`                    | the end is below the start                                  |
| `EmptyPatternSet`        | `globstar([])`             | a union needs at least one pattern (§14.1)                  |
| `NegatedInUnion`         | `Glob::union(["a", "!b"])` | Rust only: a union rejects `!` members (§14.1)              |

A stray `]`, `}`, `(` or `)` is not an error; it is a literal byte (§9.1).

`TooComplex` bounds the work of compiling. Braces whose branches cross a `/` can make the segments of a pattern multiply: `{a,b,/}` repeated k times has about k² distinct segments. An implementation MAY fail such a pattern with `TooComplex`, and MUST NOT fail one whose compile work stays linear in its length. The bound and how work is counted are implementation-defined; `max` reports the bound that was passed. (Informative: the reference implementations allow 64 units of work per unit of pattern size, and at least 2^20 units. Ordinary patterns and big unions need one to three per unit, so only the multiplying shapes reach the bound, at a few hundred bytes for the shape above.)

The parser SHOULD be lenient in the following cases:

| Case                      | Example        | Meaning                                                   |
| ------------------------- | -------------- | --------------------------------------------------------- |
| `\X` with an ordinary `X` | `\a`           | `a`                                                       |
| `-` at a class edge       | `[a-]`, `[-a]` | `a` or `-`                                                |
| single-branch brace       | `{a}`          | the literal `{a}` (§7.4)                                  |
| `**` not owning a segment | `a**b`         | `a*b` (§8.1)                                              |
| consecutive `**/**`       | `a/**/**/b`    | `a/**/b` (§8.6)                                           |
| repeated `/`              | `a//b`         | kept as written; only `**` boundaries are lenient (§12.3) |

The reference error types follow. `at` is a byte offset into the UTF-8 pattern (for `InvalidRange`, where the range starts). An error that one member of a pattern list raises names the member by its position in the list, counting every member given: Rust wraps it in `InMember`, JS adds `index` and keeps the member's `kind`.

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
    NegatedInUnion { index: usize, pattern: String }, // Rust only (§14.1)
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

## 11. Options

There are exactly two options. Every syntactic construct is always on and has no toggle (§11.3).

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

### 11.1 `dot`

Bash-style dot protection: whether `*`, `?` and `[^...]` may consume a `.` at a segment start. The precise positions are defined in §12.4.

The default at the matcher layer is `true`. The walker layer defaults to `false` (WALKER_SPEC.md §3).

| Setting       | `*.txt` on `.txt` | `**/*` on `a/.b` | To match a dotfile                               |
| ------------- | ----------------- | ---------------- | ------------------------------------------------ |
| `dot = false` | no match          | no match         | name the dot (`.*`, `[.]*`) or pass `dot = true` |
| `dot = true`  | match             | match            |                                                  |

`dot = false` is the bash and picomatch behavior.

> **Rationale.** The matcher is a pure pattern-to-path predicate; hidden files are a filesystem convention, not a property of the pattern language. The convention therefore lives in the walker's default.

### 11.2 `case_insensitive`

When set, ASCII letters compare case-folded: `*.ts` matches `MAIN.TS`; `[A-Z]` matches `a`. The default is `false` at both layers.

Case folding is a matching behavior, not a syntax rule. The parser MUST NOT consult the flag, so every validity check stays byte-level and case-sensitive: `[a-A]` is `InvalidRange` even under `case_insensitive = true`, and error messages always describe the user's literal input.

Folding covers ASCII only (§12.5). Non-ASCII bytes compare verbatim: `café.txt` matches `CAFé.txt` but not `CAFÉ.txt`. Callers who need Unicode folding MUST normalize both pattern and path first (for example NFKC plus lowercase).

See WALKER_SPEC.md §5 for how this option interacts with the walker's static-prefix seeding.

### 11.3 Toggles deliberately not provided

| Considered        | Status          | Rationale                             |
| ----------------- | --------------- | ------------------------------------- |
| `brace_expansion` | always on       | write `\{a,b\}` for one literal brace |
| `globstar`        | always on       | no real-world need                    |
| `unicode_case`    | not implemented | callers normalize first               |

> **Rationale.** Each toggle would add parser state for a case that escapes already express.

---

## 12. Match Semantics

### 12.1 Definition of "match"

`is_match(P, path) := path ∈ L(P)`, where `L(P)` is the language defined in §§4–9.

### 12.2 Whole-path matching

A match covers the whole path: every byte of the path is consumed by `P`. There is no substring matching.

### 12.3 Path separator handling

#### Pattern side

`/` is the only segment delimiter and `\` is always the escape (§2.2, §2.4). Every structural rule (segment ownership, `**` crossing, class locality, the dot-protection trigger) is evaluated against `/` in the pattern.

#### Path side

The matcher recognizes the separator set `Seps` of §2.3 wherever it asks whether a byte is a separator: when a pattern `/` consumes a path byte, when `**` crosses segments, when a segment start is found for dot protection (§12.4), and when a class refuses a separator (§6.2).

#### Strict and lenient separators

Each `/` in a pattern consumes exactly one separator byte: `a/b` does not match `a//b`, and `a//b` requires two separators. Separator runs are accepted in exactly two places:

- the `/` on either side of a `**`: `a/**/b` matches `a//b`, as in picomatch and globset. This holds across braces too: `{a,/**}/{**/x,b}` matches `a//x`;
- a pattern-leading `**/`, which also accepts zero or more leading separators: `**/foo` matches `foo`, `/foo` and `//server/share/foo` (§8.5).

#### What the matcher does not do

1. Rewrite `\` to `/`: a separator byte is recognized, never substituted.
2. Collapse `//` in a pattern: each `/` is its own strict separator.
3. Collapse `//` in a path: only the two lenient places above absorb runs.
4. Resolve `.` or `..`: they are matched as bytes.
5. Resolve symlinks: a walker-layer concern.

> **Note.** The same pattern and path MAY match on one platform and not on another, because `Seps` differs. This is the cost of accepting native paths. Callers who need portable results SHOULD normalize paths to `/` first.

### 12.4 Precise dot-protection semantics

Under `dot = false`, a path position `i` is dot-protected when `path[i] == '.'` and either `i == 0` or `path[i-1]` ∈ `Seps`: the `.` that starts a segment. A segment that starts there matches only a pattern segment whose first element takes the `.` itself:

- a literal `.`, or a positive class that lists `.` (a range counts: `[,-.]`), takes it as usual;
- a `*` there matches nothing, not even the empty string: `*.txt` does not match `.txt`, and `*.*` does not match `.a`;
- `?` and a negated class do not match the `.`;
- braces are judged per expansion (§7.0): `{*,.}a` matches `.a` through its expansion `.a`, and `*{.a,b}` does not;
- `**` itself is unaffected, but every segment start it exposes is subject to the rule: `**` does not match `a/.b` or `.a/b`.

### 12.5 Multi-byte characters

Matching is byte-wise. `?` consumes one byte, not one code point: to match one three-byte CJK character, write `???`. This is an acknowledged non-goal.

---

## 13. Directory Matching `match_dir`

### 13.1 Definition

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

The result answers three questions: does `d` match (`matches`), may something below it match (`may_match_below`), and does everything below it match (`matches_all_below`, §13.6). The third implies the second.

The names read from the pattern's side; what a walker does depends on the pattern's role. A walker that selects files descends into `d` while `may_match_below` holds. A walker that excludes files skips `d` whole once `matches_all_below` holds, and stops testing under `d` once `may_match_below` is false.

### 13.2 Semantics

```
below(d)         := Σ⁺                                if d = ""   (the walk root)
                    { d ++ "/" ++ w : w ∈ Σ* }        otherwise

matches(P, d)    := d ∈ L(P)
may_below(P, d)  := below(d) ∩ L(P) ≠ ∅
all_below(P, d)  := §13.6

match_dir(P, d) :=
  match (matches, may_below, all_below):
    (true,  _,     true ) → DescendAllAndMatch
    (false, _,     true ) → DescendAll
    (true,  true,  false) → DescendAndMatch
    (true,  false, false) → Match
    (false, true,  false) → Descend
    (false, false, false) → Pruned
```

Both flags about "below" are one-sided (§13.6). A `false` may-below is a guarantee that nothing below matches; `true` MAY be conservative. The reference implementations report may-below at the root for every pattern, since nothing has been consumed there.

`d` is taken as given and never canonicalized. `match_dir("src", "src")` is `Match`. A trailing slash asks about that exact byte string: `match_dir("a/", "a/**")` is `DescendAndMatch` (`a/` ∈ L and `a//x` ∈ L), while `match_dir("a/", "a/b")` is `Pruned` (nothing in `{a/b}` starts with `a//`). Walkers get directory paths from `readdir` without a trailing separator, so the slashless form is the canonical query.

### 13.3 Examples

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

These are the `dot = false` results. Under `dot = true` the rows whose pattern ends in `/**` and whose directory is at or below that `/**` report the covering results instead: `src/components/**` gives `DescendAll` on `src/components` and `DescendAllAndMatch` on `src/components/a`; `{src,lib}/**` gives `DescendAll` on `src` (§13.6).

### 13.4 Leading `!` and `match_dir`

A negated pattern SHOULD return `Descend` for every `d`, and never a covering result (§13.6). The reference implementations always do. In a union (§14.3) a negated member contributes exactly this `Descend`.

> **Rationale.** Inverting the body's result would tell a walker almost nothing: `!P` normally means "everything except P", and the walker must visit the whole tree to find it.

### 13.5 Implementation hints

> **Implementation note.** Feed `d` to the matcher and classify where it stops: no live state is `Pruned`; an accepting state is `Match` or `DescendAndMatch`; live states without acceptance are `Descend`. For the all-below flag, continue from those states as §13.6 describes.
>
> The same analysis yields `static_prefixes`: for each expansion of the pattern's braces (§7.0), its literal head up to its last whole segment, with heads a shorter one covers removed. Every match starts with one of them, compared byte for byte with `/` as the separator and in the pattern's case (a case-insensitive match, or a Windows path with `\`, may differ there). Walkers seed traversal from them (WALKER_SPEC.md §6). The reference implementations bound the work linearly in the pattern; past the bound a head ends at its last whole segment, which is a shorter prefix and still holds.

### 13.6 All-below: `DescendAll` and `DescendAllAndMatch`

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

The reference implementations decide the flag by continuing from the state `d` leaves the matcher in and feeding it every wildcard-only path: a path whose bytes are consumed by `?`, `*` and `**` alone. Literals and classes, negated classes included, take none of them. The flag is set when every such path in `R` is accepted. A real byte is accepted wherever a wildcard-only byte is, which gives the guarantee.

Consequences:

- Under `dot = false` the flag is never set: `**` does not match `d/.cache`, so no subtree is covered.
- A pure literal pattern never sets it, and neither does a negated pattern (§13.4).
- A union sets it when its members cover the subtree together, not only when one member does (§14.3).

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
- coverage by name length across separator-crossing branches: `?/**/?` with `**/??*` takes every name below `c/x.js`, as a one-byte name or a longer one.

The rule asks a single element to take every name, and neither shape has one.

The reference implementations also bound the proof. They feed at most 64 wildcard-only segments, and a matcher whose state has not settled by then leaves all-below unset: the union of `**` and `**/` + `*/` × 100 + `x` covers everything below `a`, and is not proven. For a single path with one `**`, they spend at most 4096 element compares on the tail positions a directory may leave; past that they set may-below and leave all-below unset, which are the safe answers of both flags.

---

## 14. Multi-pattern Matchers

### 14.1 Definition

A multi-pattern matcher is the boolean OR of N patterns. In Rust, `Glob::union` and `Glob::union_with` compile the members into one `Glob`; there is no separate set type. In JS, `globstar(patterns)` returns a predicate and `compileMatcher(patterns)` returns a matcher with `match`, `matchDir` and `staticPrefixes`.

```rust
// Rust
impl Glob {
    pub fn union<I, S>(patterns: I) -> Result<Glob, GlobError>;
    pub fn union_with<I, S>(patterns: I, opts: CompileOptions) -> Result<Glob, GlobError>;
}
```

```ts
// JS
function globstar(patterns: string | readonly string[], opts?: GlobstarOptions): (input: string) => boolean;
function compileMatcher(patterns: string | readonly string[], opts?: GlobstarOptions): Matcher;
```

A union answers the boolean questions only (`is_match`, `match_dir`, `static_prefixes`); which member matched is not part of the API. An empty list is `EmptyPatternSet`. A pattern given twice counts once. All members share one expansion budget (§7.7).

Negated members differ by language. Rust rejects them (`NegatedInUnion`, §10). JS accepts them, and each contributes its complement to the OR: `["src/**", "!**/*.test.ts"]` matches `lib/a.ts`, because the second member does. A `!` member never subtracts from the others; include/exclude filtering belongs to the walker (WALKER_SPEC.md §4).

### 14.2 Relationship to a single glob

```
union.is_match(path) ≡ ∃ g ∈ members.  g.is_match(path)
```

### 14.3 Combined `match_dir`

```
union.match_dir(d) := combine(
  ∃ g. matches(g, d),
  ∃ g. may_below(g, d),
  all_below(⋃ L(g), d) )
```

with `combine` the table of §13.2. If any member may match below `d`, the union may: it MUST NOT report `Pruned` unless every member does. A negated member (JS) contributes `Descend` (§13.4).

All-below is a property of the union's language (§13.6): one covering member is enough, and members that each cover a part cover together (`a/*` the children, `a/*/**` everything deeper).

### 14.4 Literal acceleration

An implementation MAY use literal facts (prefix, suffix, substring, exact) to reject paths before running the full matcher. See [`../theory/05-literal-prefilter.md`](../theory/05-literal-prefilter.md) and [`../theory/06-walker-and-pruning.md`](../theory/06-walker-and-pruning.md) for the facts the reference implementations build on.

---

## 15. Edge Case Catalog

This section previews the executable corpus in `crates/globstar/tests/corpus/`; every line below has one or more rows there. `dot=false` is written out where it matters; the matcher's default is `dot=true` (§11.1).

### 15.1 Basic wildcards

```
*              path=""          ⇒ true
*              path="a"         ⇒ true
*              path="/"         ⇒ false
*              path="a/b"       ⇒ false
*.txt          path="a.txt"     ⇒ true
*.txt          path=".txt"      ⇒ false  (dot=false)
?.txt          path="a.txt"     ⇒ true
?.txt          path=".txt"      ⇒ false  (dot=false)
```

### 15.2 `**`

```
**             path=""          ⇒ true
**             path="a"         ⇒ true
**             path="a/b/c"     ⇒ true
a/**           path="a"         ⇒ false
a/**           path="a/"        ⇒ true
a/**           path="a/b/c"     ⇒ true
a/**/b         path="a/b"       ⇒ true
a/**/b         path="a/x/y/b"   ⇒ true
**/b           path="b"         ⇒ true
**/b           path="a/b"       ⇒ true
**/**/b        path="a/b"       ⇒ true   (consecutive `**` collapse)
```

### 15.3 `**` degradation

```
a**b           path="ab"        ⇒ true   (a*b)
a**b           path="axb"       ⇒ true
a**b           path="a/b"       ⇒ false
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

### 15.5 Braces

```
{a,b}          path="a"         ⇒ true
{a,b}          path="c"         ⇒ false
{a,}           path="a"         ⇒ true
{a,}           path=""          ⇒ true
{a,{b,c}}      path="b"         ⇒ true
{a,b}{c,d}     path="ad"        ⇒ true
{a}            path="{a}"       ⇒ true   (single branch is literal)
```

### 15.6 Escapes

```
\*             path="*"         ⇒ true
\*             path="a"         ⇒ false
\\             path="\"         ⇒ true
\a             path="a"         ⇒ true   (lenient)
foo\           ⇒ Err(TrailingBackslash)
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
.*             path=".hidden"   ⇒ true   (dot=false)
*              path=".hidden"   ⇒ true   (dot=true)
**/*           path="a/.b"      ⇒ false  (dot=false)
**/*           path="a/.b"      ⇒ true   (dot=true)
*.txt          path=".txt"      ⇒ false  (dot=false)
*.*            path=".a"        ⇒ false  (dot=false)
```

### 15.9 Directory matching (`match_dir`)

```
src/**/*.ts    dir="src"               ⇒ Descend
src/**/*.ts    dir="src/components"    ⇒ Descend
src/**/*.ts    dir="tests"             ⇒ Pruned
src/**/*.ts    dir="node_modules"      ⇒ Pruned
{src,lib}/**   dir="src"               ⇒ Descend   (dot=false)
{src,lib}/**   dir="docs"              ⇒ Pruned
**/*.ts        dir="anything"          ⇒ Descend
```

All-below (`dot=true`, §13.6):

```
/p/dist/**          dir="/p/dist"              ⇒ DescendAll
**/node_modules/**  dir="/p/node_modules"      ⇒ DescendAll
**/node_modules/**  dir="/p/node_modules/pkg"  ⇒ DescendAllAndMatch
/p/dist/**/*        dir="/p/dist"              ⇒ DescendAll
/p/dist/**/*.js     dir="/p/dist"              ⇒ Descend
**/*.log            dir="/p/foo.log"           ⇒ DescendAndMatch
/p/dist/**          dir="/p/dist"              ⇒ Descend   (dot=false)
```

### 15.10 Error cases

```
""             ⇒ Err(Empty)
"["            ⇒ Err(UnterminatedClass)
"[]"           ⇒ Err(UnterminatedClass)   (first `]` is a member; no closer)
"[!]"          ⇒ Err(UnterminatedClass)
"[]]"          ⇒ Ok: class {`]`}
"[]-]"         ⇒ Ok: class {`]`, `-`}
"{a,b"         ⇒ Err(UnterminatedBrace)
"foo\"         ⇒ Err(TrailingBackslash)
"a\/b"         ⇒ Err(EscapedSeparator)
"[z-a]"        ⇒ Err(InvalidRange)
```

---

## 16. Differences from Other Tools

| Behavior                                  | globstar                            | Bash               | picomatch              | globset                                | fast-glob                  | doublestar    |
| ----------------------------------------- | ----------------------------------- | ------------------ | ---------------------- | -------------------------------------- | -------------------------- | ------------- |
| `dot` default                             | `true` (matcher) / `false` (walker) | `false`            | `false`                | `true` (no concept)                    | `true` (no concept)        | `false`       |
| `case_insensitive`                        | ASCII only, default `false`         | `shopt nocaseglob` | `nocase` option        | `case_insensitive` builder             | `caseSensitiveMatch=false` | Unicode-aware |
| `**` must own a segment                   | enforced                            | ✅                 | ✅                     | ✅                                     | ✅                         | ✅            |
| `a/**` matches `a`                        | ❌                                  | ⚠️                 | ⚠️                     | ❌                                     | ❌                         | ❌            |
| `**` in braces judged on expansion (§7.0) | ✅                                  | ✅ (expands first) | ❌ (branch-local `.*`) | ❌ (demotes to `*` even at boundaries) | ❌                         | n/a           |
| `**` in single-branch brace `{**}` (§7.4) | degrades to `*`                     | literal            | keeps globstar         | ⚠️                                     | keeps globstar             | ⚠️            |
| `**/X` matches absolute `/a/X` (§8.5)     | ✅                                  | n/a                | ✅                     | ✅                                     | ✅                         | ❌            |
| Extglobs                                  | ❌ (literal)                        | ✅ (`shopt`)       | ✅                     | ❌                                     | ❌                         | ✅            |
| POSIX character classes                   | ❌                                  | ✅                 | ✅                     | ❌                                     | ❌                         | ⚠️            |
| `[/]` inside a class (§6.2)               | parse error                         | literal            | literal                | literal                                | literal                    | ⚠️            |
| POSIX first-`]` rule (§6.5)               | ✅                                  | ✅                 | ✅                     | ❌                                     | ❌                         | ⚠️            |
| `[!a]` ≡ `[^a]`                           | ✅ (both negate)                    | ✅                 | ❌ (only `[^a]`)       | ⚠️                                     | ❌ (only `[^a]`)           | ⚠️            |
| Brace ranges `{1..10}`                    | ❌                                  | ✅                 | ✅                     | ❌                                     | ❌                         | ❌            |
| Brace nesting                             | ≤ 32                                | ✅                 | ✅                     | ✅                                     | ≤ 10                       | ✅            |
| Leading-`!` negation                      | ✅                                  | n/a                | ✅                     | ⚠️                                     | ✅                         | ✅            |
| `/` required in patterns                  | ✅                                  | n/a                | ⚠️                     | ⚠️                                     | ⚠️                         | ⚠️            |
| `\` is always the escape                  | ✅                                  | ✅                 | ✅                     | ⚠️                                     | ⚠️                         | ⚠️            |
| `\` in a path is a separator (§12.3)      | platform-conditional                | ❌                 | ✅ (JS paths)          | platform                               | ❌                         | platform      |
| Stray `]`, `}`, `)`                       | literal                             | literal            | literal                | literal                                | literal                    | literal       |
| Unclosed constructs                       | parse error                         | literal            | ⚠️                     | error                                  | error                      | literal       |
| `match_dir` API                           | ✅ (first-class)                    | n/a                | ❌                     | ✅                                     | ❌                         | ❌            |

Symbols: ✅ yes, ❌ no, ⚠️ partial or conditional, n/a not applicable.

> **Note.** This dialect is closest to picomatch and doublestar. It is stricter about input (`/` is mandatory in patterns), treats `match_dir` as a first-class operation, and is stricter about syntax errors: an unterminated class or brace is a hard error, never silently literal.

---

## 17. Appendix A: Version History

- **v0.1** (2026-04-11): initial draft.
- **v0.1.1** (2026-04-11): §8 rewritten. `L(**)` is any byte string (regex `.*`); `L(a/**)` includes `a/` but not `a`; §8.4 added.
- **v0.1.2** (2026-04-14): class `/` is `UnterminatedClass` (§6.2); POSIX first-`]` rule (§6.5), `EmptyClass` and `EmptyNegationClass` removed; leading `**/` means "at any depth" (§8.5).
- **v0.1.3** (2026-04-14): implementation-defined separator set `Seps` (§12.3).
- **v0.1.4** (2026-04-14): matcher `dot` default flipped to `true`; walker stays `false`.
- **v0.1.5** (2026-04-14): class `Seps` guard applies to both polarities.
- **v0.1.6** (2026-04-14, rolled back): stray `]` and `}` as errors. Reverted: real file names need them as plain bytes more often than typo detection helps.
- **v0.1.7** (2026-04-14): ASCII `case_insensitive` option (§11.2).
- **v0.2.0** (2026-04-23): extglobs removed; `@()`, `?()`, `*()`, `+()`, `!(...)` are literal.
- **v0.2.1** (2026-07-23): braces defined by the expansion equation (§7.0). `**` ownership is judged on the expanded form: `a{**,x}b` degrades, `{**,x}/b` matches `b`, `a/{**/x,y}` gets the lenient boundary. Deliberate divergence from picomatch (§16). Corpus group `brace.globstar-expansion`.
- **v0.2.2** (2026-09-28): `DescendAll` and `DescendAllAndMatch` (§13.1, §13.2, §13.6): the one-sided guarantee, the wildcard-only rule, the shapes it cannot prove; never set under `dot=false`. Corpus group `all-below`.
- **v0.2.3** (2026-09-30): the expansion equation holds on every shape (§7.0, §8.1): `**{/a,b}` ≡ `**/a ∪ *b`, `{a,}**` ≡ `a* ∪ **`. The local expansion is bounded by `BraceExpansionTooLarge` and `BraceNestingTooDeep` (§7.6, §7.7, §10).
- **v0.2.4** (2026-10-14): editorial pass; the text follows the implementation everywhere, and the document states its conventions.
  - `|` is a literal byte everywhere (§3, §5); `[[:alpha:]]` compiles as an ordinary class (§6.6).
  - Removed the "trailing `/` means directory" claims (§8.3, §16): no such rule exists.
  - §7.6: the nesting counter sees every `{`, single-branch ones included.
  - §13.2 defines the walk root (`d = ""`); §13.4: a negated pattern is always `Descend`.
  - §10: the JS `TooManyStates` note is gone with the engine that had the cap; `NegatedInUnion` carries `index` and `pattern`.
  - Implementation-internal names left the normative text. RFC 2119 key words are reserved for requirements; rationale and implementation notes are marked as informative.
- **v0.2.5** (2026-10-16): `TooComplex` (§10) bounds the compile work of braces whose segments multiply.
- **v0.2.6** (2026-10-16): §13.6 states how the reference implementations bound the all-below proof (64 wildcard-only segments, 4096 tail compares); §7.2 and §7.7 point at `TooComplex`.
- **v0.2.8** (2026-10-16): §10: `InvalidRange.at` is where the range starts; an error one member of a pattern list raises names the member (`InMember` in Rust, `index` in JS).
- **v0.2.7** (2026-10-16): §12.4 states dot protection as the implementations apply it, per segment start: a `*` there matches nothing, not even the empty string (`*.txt` does not match `.txt`); §15.8 shows it.
- Awaiting owner approval before freezing as v1.0.

---

## 18. Appendix B: Glossary

| Term                   | Meaning                                                                                                                  |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Pattern                | The user-supplied glob string.                                                                                           |
| Path                   | The byte sequence being matched.                                                                                         |
| Segment                | A piece of a path between separators.                                                                                    |
| `Seps`                 | The implementation-defined set of separator bytes (§2.3, §12.3). `/` is always in it.                                    |
| Globstar               | A `**` that owns a whole segment (§8.1). Any other `**` is a star.                                                       |
| Dot-protected position | A `.` at the start of the path or right after a separator (§12.4). Under `dot = false` no wildcard consumes it.          |
| `match_dir` results    | `Match`, `Pruned`, `Descend`, `DescendAndMatch`, `DescendAll`, `DescendAllAndMatch` (§13).                               |
| All-below              | Every path below a directory matches (§13.6). The two `DescendAll…` results carry it.                                    |
| Union                  | The boolean OR of N patterns: `Glob::union` in Rust, `globstar(patterns)` and `compileMatcher(patterns)` in JS (§14).    |
| Literal facts          | Byte facts every match of a pattern must satisfy (prefix, suffix, substring, exact), used to reject paths early (§14.4). |
| Static prefixes        | Each expansion's literal head up to its last whole segment; every match starts with one (§13.5). Walkers seed from them. |
