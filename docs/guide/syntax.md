# Pattern syntax

This page lists every construct of a pattern, what each one matches, and which characters must be escaped. The `**` wildcard, character classes, and braces each have their own page with more detail: see [The `**` wildcard](./globstar.md), [Character classes](./character-classes.md), and [Brace expansion](./braces.md).

All examples use the default options (`dot: true`, `caseInsensitive: false`). See [How a path is matched](./matching.md) for how `dot` and case-insensitivity change the results.

## Quick reference

| Construct             | Matches                                           | Example       |
| --------------------- | ------------------------------------------------- | ------------- |
| `abc`                 | exactly these characters                          | `README.md`   |
| `?`                   | one character, never `/`                          | `file?.txt`   |
| `*`                   | zero or more characters inside one path segment   | `*.ts`        |
| `**`                  | zero or more whole segments (it crosses `/`)      | `src/**/*.ts` |
| `[abc]`, `[a-z]`      | one character from a set                          | `*.[jt]s`     |
| `[!abc]`, `[^abc]`    | one character not in the set                      | `[!_]*.ts`    |
| `{a,b}`               | any one of the branches                           | `*.{js,ts}`   |
| `\X`                  | the character `X` itself                          | `\*.md`       |
| `!` at the very start | everything the rest of the pattern does not match | `!*.test.ts`  |

## Three rules

- A pattern matches the whole path, never a part of it.
- Matching is byte by byte in UTF-8: `?` and a class take one byte, and a character outside ASCII is a literal (`docs/日本語.md` matches only that path).
- In a pattern, `/` is the only separator on every platform, and `\` is always an escape.

[How a path is matched](./matching.md) explains each rule with examples, and what `\` means on the path side.

## Meta-characters

These characters can have a special meaning: `*` `?` `[` `]` `{` `}` `,` `\`, plus `!` at the very start of a pattern. Some are special everywhere; others only in one place.

| Character                | Special when                                | Otherwise |
| ------------------------ | ------------------------------------------- | --------- |
| `*`, `?`                 | always (wildcards)                          | —         |
| `[`                      | always: opens a class, which must be closed | —         |
| `]`                      | it closes an open class                     | literal   |
| `{`                      | always: opens a brace, which must be closed | —         |
| `}`                      | it closes an open brace                     | literal   |
| `,`                      | inside a brace (splits branches)            | literal   |
| `\`                      | always (escapes the next character)         | —         |
| `!`                      | it is at the very start of the pattern      | literal   |
| `(`, `)`, `@`, `+`, `\|` | never                                       | literal   |

A closer with nothing open is literal: `a]b` matches the path `a]b`, and `a}b` matches `a}b`. An opener with no closer is an error: `[a` and `{a` both fail to compile. See [Compile-time errors](./errors.md).

Spec: [GLOB_SPEC §4.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Basic constructs

### Literals

Any character that is not a meta-character matches itself, exactly once. `README.md` matches only `README.md`.

### `?` — one character

`?` matches exactly one character that is not a separator. It never matches an empty string.

| Pattern     | Path         | Match? |
| ----------- | ------------ | ------ |
| `file?.txt` | `file1.txt`  | yes    |
| `file?.txt` | `file10.txt` | no     |
| `file?.txt` | `file.txt`   | no     |
| `a?b`       | `a/b`        | no     |

### `*` — anything inside one segment

`*` matches zero or more characters, but it stops at `/`. It can match nothing at all.

| Pattern    | Path            | Match? |
| ---------- | --------------- | ------ |
| `foo*bar`  | `foobar`        | yes    |
| `foo*bar`  | `foo-bar`       | yes    |
| `foo*bar`  | `foo/bar`       | no     |
| `src/*.ts` | `src/main.ts`   | yes    |
| `src/*.ts` | `src/util/x.ts` | no     |

With `dot: false`, `?` and `*` do not match a `.` at the start of a segment, so `*.ts` no longer matches `.ts`. See [How a path is matched](./matching.md).

### `**` — any depth

`**` is the only construct that crosses `/`. It must stand alone as a whole segment, like `src/**/*.ts`. Written next to other characters, as in `src/**.ts`, it acts like a single `*`. The rules are on [The `**` wildcard](./globstar.md) page.

| Pattern       | Path         | Match? |
| ------------- | ------------ | ------ |
| `src/**/*.ts` | `src/a.ts`   | yes    |
| `src/**/*.ts` | `src/a/b.ts` | yes    |
| `src/**.ts`   | `src/a/b.ts` | no     |

### The empty pattern

The empty string `""` is not a valid pattern. Compiling it throws a `GlobError` with kind `Empty`.

Spec: [GLOB_SPEC §7](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## What is not supported

The following shell and glob-library syntax is not part of the dialect. Each form still compiles, with a plain meaning.

- **Extglobs** such as `@(a|b)`, `+(x)`, `*(x)`, `?(x)`, `!(x)`. The parentheses, `@`, and `+` are plain characters. A `*` or `?` inside still works as a wildcard. Use a brace like `{foo,bar}.js` instead.
- **POSIX classes** such as `[[:alpha:]]`. This is not a "letter" class, so it does not match `a`. Write `[a-zA-Z]` instead.
- **Brace ranges** such as `{1..10}` or `{a..z}`. A brace needs a comma, so `{1..3}.txt` is literal. Write `{1,2,3}` or `[1-3]` instead.
- **Regex features**: groups, backreferences such as `\1`, lookahead, lookbehind.
- **Shell expansion**: `~` and `$VAR` are plain characters. Expand them yourself before you compile.

| Pattern       | Path          | Match? |
| ------------- | ------------- | ------ |
| `+(foo).js`   | `foo.js`      | no     |
| `+(foo).js`   | `+(foo).js`   | yes    |
| `*(x).js`     | `abc(x).js`   | yes    |
| `[[:alpha:]]` | `a`           | no     |
| `{1..3}.txt`  | `1.txt`       | no     |
| `{1..3}.txt`  | `{1..3}.txt`  | yes    |
| `file(1).txt` | `file(1).txt` | yes    |
| `~/x`         | `~/x`         | yes    |

Spec: [GLOB_SPEC §1.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Escaping with `\`

A backslash makes the next character literal. This works on any character: `\*` is a literal `*`, and `\a` is just `a`. Two forms are errors:

- `\/` throws `EscapedSeparator`. A file name can never contain `/`, so this could never match.
- A `\` at the very end, as in `foo\`, throws `TrailingBackslash`.

| Pattern        | Path         | Match? |
| -------------- | ------------ | ------ |
| `\*.md`        | `*.md`       | yes    |
| `\*.md`        | `README.md`  | no     |
| `what\?.txt`   | `what?.txt`  | yes    |
| `what\?.txt`   | `whatx.txt`  | no     |
| `\[draft\].md` | `[draft].md` | yes    |
| `[draft].md`   | `d.md`       | yes    |
| `\\`           | `\`          | yes    |

In the last two rows, `[draft]` without escapes is a class that matches one letter.

### What needs escaping, and where

| To match a literal         | Write                      | Why                                                         |
| -------------------------- | -------------------------- | ----------------------------------------------------------- |
| `*` or `?`                 | `\*`, `\?`                 | they are always wildcards                                   |
| `[`                        | `\[`                       | a bare `[` opens a class                                    |
| `{`                        | `\{`                       | a bare `{` opens a brace; `\{a,b}` matches the path `{a,b}` |
| `]` or `}`                 | `]`, `}` (escape optional) | literal when nothing is open                                |
| `,` inside a brace         | `\,`                       | a bare `,` splits branches; `{a\,b,c}` matches `a,b`        |
| `\`                        | `\\`                       | a bare `\` escapes the next character                       |
| `!` as the first character | `\!`                       | a leading `!` negates the pattern                           |

::: tip Backslashes in source code
In a JavaScript string, `\` is also an escape, so the pattern `\*.md` is written `'\\*.md'` (or `String.raw` with `` `\*.md` ``). In Rust, use a raw string: `r"\*.md"`.
:::

Spec: [GLOB_SPEC §4.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Negation with a leading `!`

A `!` at the very start of a pattern flips the result: the pattern matches every path the rest of it does **not** match. globstar counts the whole run of leading `!`. An odd count negates; an even count cancels out.

| Pattern            | Path           | Match? |
| ------------------ | -------------- | ------ |
| `!*.test.ts`       | `app.test.ts`  | no     |
| `!*.test.ts`       | `app.ts`       | yes    |
| `!!*.ts`           | `a.ts`         | yes    |
| `!!!*.ts`          | `a.ts`         | no     |
| `!node_modules/**` | `src/index.js` | yes    |

Negation always applies to the whole pattern. There is no way to negate just one segment, and a `!` anywhere else is a plain character:

| Pattern             | Path                | Match? |
| ------------------- | ------------------- | ------ |
| `src/!important.md` | `src/!important.md` | yes    |
| `!(foo)`            | `foo`               | yes    |
| `!(foo)`            | `(foo)`             | no     |
| `\!foo`             | `!foo`              | yes    |
| `[\!]foo`           | `!foo`              | yes    |

`!(foo)` is not an extglob: it is "not the literal path `(foo)`". To match a name that starts with `!`, escape it (`\!foo`) or put it in a class (`[\!]foo`). Do not write `[!]foo`: there the `]` is a class member, the class never closes, and the pattern fails to compile.

The leading `!` is read before braces are expanded, so a `!` inside a brace is literal: `{!a,b}` matches `!a` and `b`, but not `a`.

::: warning `!` in a list of patterns
A list is an OR, so a `!` member adds paths instead of removing them. See [Negated members](./multi-pattern.md#negated-members).
:::

Spec: [GLOB_SPEC §11](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
