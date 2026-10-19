# Pattern syntax

This page lists every construct a globstar pattern can use, what each one matches, and which characters you need to escape. The `**` wildcard, character classes, and braces each have their own page with more detail: see [The `**` wildcard](./globstar.md), [Character classes](./character-classes.md), and [Brace expansion](./braces.md).

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

## A pattern matches the whole path

A pattern must match the entire path, from the first character to the last. It never matches just a part of it. To match a file at any depth, start the pattern with `**/`.

| Pattern        | Path                        | Match? |
| -------------- | --------------------------- | ------ |
| `*.ts`         | `index.ts`                  | ✅     |
| `*.ts`         | `src/index.ts`              | ❌     |
| `**/*.ts`      | `src/index.ts`              | ✅     |
| `node_modules` | `packages/web/node_modules` | ❌     |
| `src`          | `src/main.ts`               | ❌     |

Spec: [GLOB_SPEC §14.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Characters and bytes

A pattern is a normal string. globstar compares it with the path byte by byte, using UTF-8. Characters outside ASCII are always plain literals, so `docs/日本語.md` matches only the path `docs/日本語.md`.

Because matching is byte-based, `?` matches one **byte**, not one visible character. A character such as `日` takes three bytes in UTF-8, so you need `???` to match it:

| Pattern     | Path       | Match? |
| ----------- | ---------- | ------ |
| `?.md`      | `日.md`    | ❌     |
| `???.md`    | `日.md`    | ✅     |
| `caf?.txt`  | `café.txt` | ❌     |
| `caf??.txt` | `café.txt` | ✅     |

The same rule applies to character classes. See [Character classes](./character-classes.md).

Spec: [GLOB_SPEC §2.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## `/` is the only separator in a pattern

Inside a pattern, `/` is the only path separator, on every platform, Windows included. A `\` in a pattern is always an escape, never a separator. So `src\main.rs` does not mean "`main.rs` inside `src`": it is `src`, then an escaped `m`, then `ain.rs`.

| Pattern       | Path          | Match? |
| ------------- | ------------- | ------ |
| `src/main.rs` | `src/main.rs` | ✅     |
| `src\main.rs` | `src/main.rs` | ❌     |
| `src\main.rs` | `srcmain.rs`  | ✅     |

If your tool takes patterns from a Windows shell, turn `\` into `/` before you compile them. The **path** you match is a different story: on Windows it may use `\`. See [How a path is matched](./matching.md).

Each `/` in a pattern matches exactly one separator, so `a/b` does not match `a//b`. The one exception is a `/` next to `**`, which also accepts repeated separators (see [The `**` wildcard](./globstar.md)).

Spec: [GLOB_SPEC §2.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

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

So a stray closer is fine: `a]b` matches the path `a]b`, and `a}b` matches `a}b`. An opener with no closer is an error: `[a` and `{a` both fail to compile. See [Compile-time errors](./errors.md).

Spec: [GLOB_SPEC §5.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Basic constructs

### Literals

Any character that is not a meta-character matches itself, exactly once. `README.md` matches only `README.md`.

### `?` — one character

`?` matches exactly one character that is not a separator. It never matches an empty string.

| Pattern     | Path         | Match? |
| ----------- | ------------ | ------ |
| `file?.txt` | `file1.txt`  | ✅     |
| `file?.txt` | `file10.txt` | ❌     |
| `file?.txt` | `file.txt`   | ❌     |
| `a?b`       | `a/b`        | ❌     |

### `*` — anything inside one segment

`*` matches zero or more characters, but it stops at `/`. It can match nothing at all.

| Pattern    | Path            | Match? |
| ---------- | --------------- | ------ |
| `foo*bar`  | `foobar`        | ✅     |
| `foo*bar`  | `foo-bar`       | ✅     |
| `foo*bar`  | `foo/bar`       | ❌     |
| `src/*.ts` | `src/main.ts`   | ✅     |
| `src/*.ts` | `src/util/x.ts` | ❌     |

With `dot: false`, `?` and `*` do not match a `.` at the start of a segment, so `*.ts` no longer matches `.ts`. See [How a path is matched](./matching.md).

### `**` — any depth

`**` is the only construct that crosses `/`. It must stand alone as a whole segment, like `src/**/*.ts`. Written next to other characters, as in `src/**.ts`, it acts like a single `*`. The rules are on [The `**` wildcard](./globstar.md) page.

| Pattern       | Path         | Match? |
| ------------- | ------------ | ------ |
| `src/**/*.ts` | `src/a.ts`   | ✅     |
| `src/**/*.ts` | `src/a/b.ts` | ✅     |
| `src/**.ts`   | `src/a/b.ts` | ❌     |

### The empty pattern

The empty string `""` is not a valid pattern. Compiling it throws a `GlobError` with kind `Empty`.

Spec: [GLOB_SPEC §7](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## What is not supported

Some popular syntax from shells and other glob libraries is not part of globstar. Extglobs, brace ranges, `~`, and `$VAR` still compile, but they mean something else.

- **Extglobs** such as `@(a|b)`, `+(x)`, `*(x)`, `?(x)`, `!(x)`. The parentheses, `@`, and `+` are plain characters. A `*` or `?` inside still works as a wildcard. Use a brace like `{foo,bar}.js` instead.
- **POSIX classes** such as `[[:alpha:]]`. This is not a "letter" class, so it does not match `a`. Write `[a-zA-Z]` instead.
- **Brace ranges** such as `{1..10}` or `{a..z}`. A brace needs a comma, so `{1..3}.txt` is literal. Write `{1,2,3}` or `[1-3]` instead.
- **Regex features**: groups, backreferences such as `\1`, lookahead, lookbehind.
- **Shell expansion**: `~` and `$VAR` are plain characters. Expand them yourself before you compile.

| Pattern       | Path          | Match? |
| ------------- | ------------- | ------ |
| `+(foo).js`   | `foo.js`      | ❌     |
| `+(foo).js`   | `+(foo).js`   | ✅     |
| `*(x).js`     | `abc(x).js`   | ✅     |
| `[[:alpha:]]` | `a`           | ❌     |
| `{1..3}.txt`  | `1.txt`       | ❌     |
| `{1..3}.txt`  | `{1..3}.txt`  | ✅     |
| `file(1).txt` | `file(1).txt` | ✅     |
| `~/x`         | `~/x`         | ✅     |

Spec: [GLOB_SPEC §1.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Escaping with `\`

A backslash makes the next character literal. This works on any character: `\*` is a literal `*`, and `\a` is just `a`. Two forms are errors:

- `\/` throws `EscapedSeparator`. A file name can never contain `/`, so this could never match.
- A `\` at the very end, as in `foo\`, throws `TrailingBackslash`.

| Pattern        | Path         | Match? |
| -------------- | ------------ | ------ |
| `\*.md`        | `*.md`       | ✅     |
| `\*.md`        | `README.md`  | ❌     |
| `what\?.txt`   | `what?.txt`  | ✅     |
| `what\?.txt`   | `whatx.txt`  | ❌     |
| `\[draft\].md` | `[draft].md` | ✅     |
| `[draft].md`   | `d.md`       | ✅     |
| `\\`           | `\`          | ✅     |

Note the last two rows: without escapes, `[draft]` is a class that matches one letter.

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

Spec: [GLOB_SPEC §4.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Negation with a leading `!`

A `!` at the very start of a pattern flips the result: the pattern matches every path the rest of it does **not** match. globstar counts the whole run of leading `!`. An odd count negates; an even count cancels out.

| Pattern            | Path           | Match? |
| ------------------ | -------------- | ------ |
| `!*.test.ts`       | `app.test.ts`  | ❌     |
| `!*.test.ts`       | `app.ts`       | ✅     |
| `!!*.ts`           | `a.ts`         | ✅     |
| `!!!*.ts`          | `a.ts`         | ❌     |
| `!node_modules/**` | `src/index.js` | ✅     |

Negation always applies to the whole pattern. There is no way to negate just one segment, and a `!` anywhere else is a plain character:

| Pattern             | Path                | Match? |
| ------------------- | ------------------- | ------ |
| `src/!important.md` | `src/!important.md` | ✅     |
| `!(foo)`            | `foo`               | ✅     |
| `!(foo)`            | `(foo)`             | ❌     |
| `\!foo`             | `!foo`              | ✅     |
| `[\!]foo`           | `!foo`              | ✅     |

`!(foo)` is not an extglob: it is "not the literal path `(foo)`". To match a name that starts with `!`, escape it (`\!foo`) or put it in a class (`[\!]foo`). Do not write `[!]foo`: there the `]` is a class member, the class never closes, and the pattern fails to compile.

The leading `!` is read before braces are expanded, so a `!` inside a brace is literal: `{!a,b}` matches `!a` and `b`, but not `a`.

::: warning `!` in a list of patterns
A list is an OR, so a `!` member adds paths instead of removing them. See [Negated members](./multi-pattern.md#negated-members).
:::

Spec: [GLOB_SPEC §11](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
