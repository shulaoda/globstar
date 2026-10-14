# Character classes

A character class, written in square brackets like `[abc]`, matches exactly one character from a set. This page covers sets, ranges, negation, which characters are special inside the brackets, and the cases that fail to compile. For the rest of the syntax, see [Pattern syntax](./syntax.md).

All examples use the default options (`dot: true`, `caseInsensitive: false`) unless a table says otherwise.

## Basic forms

| Form       | Matches one character that is…                  |
| ---------- | ----------------------------------------------- |
| `[abc]`    | `a`, `b`, or `c`                                |
| `[a-z]`    | in the range `a` to `z`                         |
| `[abc0-9]` | `a`, `b`, `c`, or a digit (sets and ranges mix) |
| `[!abc]`   | not `a`, `b`, or `c`                            |
| `[^abc]`   | not `a`, `b`, or `c` (same as `[!abc]`)         |

| Pattern         | Path         | Match? |
| --------------- | ------------ | ------ |
| `*.[ch]`        | `main.c`     | ✅     |
| `*.[ch]`        | `util.h`     | ✅     |
| `*.[ch]`        | `main.cpp`   | ❌     |
| `*.[jt]s`       | `app.ts`     | ✅     |
| `[Rr]EADME.md`  | `README.md`  | ✅     |
| `[Rr]EADME.md`  | `ReadMe.md`  | ❌     |
| `file[0-9].txt` | `file1.txt`  | ✅     |
| `file[0-9].txt` | `file10.txt` | ❌     |
| `[a]`           | `a`          | ✅     |
| `[a]`           | `[a]`        | ❌     |

A class always matches exactly one character, never zero and never two. `file[0-9].txt` does not match `file10.txt`.

A class with one member is still a class: `[a]` matches `a`. Braces work differently: a brace with no comma, like `{a}`, is plain text (see [Brace expansion](./braces.md)).

Spec: [GLOB_SPEC §6.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Ranges

`[x-y]` matches any character from `x` to `y`, both included. Ranges compare byte values, so they are case-sensitive by default: `[a-z]` does not match `X`. With `caseInsensitive: true`, letters match in either case, so `[A-Z]` also matches `a`. See [How a path is matched](./matching.md).

A range must go up. If the end comes before the start, the pattern fails to compile with `InvalidRange`. This check looks at the bytes you wrote, so `[a-A]` is an error even with `caseInsensitive: true`.

| Pattern        | Result                  |
| -------------- | ----------------------- |
| `[0-9]`        | any digit               |
| `[a-zA-Z0-9_]` | a letter, digit, or `_` |
| `[a-a]`        | just `a`                |
| `[z-a]`        | `InvalidRange` error    |
| `[9-0]`        | `InvalidRange` error    |
| `[a-A]`        | `InvalidRange` error    |

Spec: [GLOB_SPEC §6.1, §11.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Negated classes

A `!` or `^` right after the `[` flips the class: it matches one character that is **not** in the set. The two forms mean exactly the same thing.

| Pattern    | Path           | Match? |
| ---------- | -------------- | ------ |
| `[!abc]`   | `d`            | ✅     |
| `[!abc]`   | `a`            | ❌     |
| `[^abc]`   | `d`            | ✅     |
| `[^abc]`   | `a`            | ❌     |
| `[!_]*.ts` | `index.ts`     | ✅     |
| `[!_]*.ts` | `_internal.ts` | ❌     |

### Negated classes and `dot`

With `dot: false`, a negated class at the start of a segment does not match a leading `.`. This keeps dot files hidden, the same way `*` and `?` do. A class that lists `.` as a member still matches it, and in the middle of a name the rule does not apply.

| Pattern        | Path             | `dot: true` | `dot: false` |
| -------------- | ---------------- | ----------- | ------------ |
| `[^x]*`        | `.env`           | ✅          | ❌           |
| `src/[^_]*.ts` | `src/.hidden.ts` | ✅          | ❌           |
| `src/[^_]*.ts` | `src/main.ts`    | ✅          | ✅           |
| `[.]*`         | `.env`           | ✅          | ✅           |
| `x[^_]`        | `x.`             | ✅          | ✅           |

Spec: [GLOB_SPEC §6.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Classes never match `/`

A class lives inside one path segment. It never matches a separator, not even a negated class: `[!x]` matches almost anything, but not `/`.

| Pattern  | Path  | Match? |
| -------- | ----- | ------ |
| `a[!x]b` | `ayb` | ✅     |
| `a[!x]b` | `a/b` | ❌     |

Because of this, a `/` inside the brackets is a compile error, `UnterminatedClass`. A `/` ends the segment, so the class never sees its `]`. This holds for `[/]`, `[a/b]`, `[^/]`, and the escaped form `[\/]`. An error here is better than a pattern that compiles but can never match.

A literal backslash is allowed as a member: `[\\]` is the class that holds `\`. On Linux and macOS it matches a `\` in a file name. On Windows, `\` is a path separator, so this class never matches there.

Spec: [GLOB_SPEC §6.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Special characters inside a class

Inside the brackets, most meta-characters lose their meaning. Only these are special:

| Character                       | Inside a class                                                          |
| ------------------------------- | ----------------------------------------------------------------------- |
| `]`                             | closes the class, unless it comes first (see below)                     |
| `-`                             | makes a range between two characters; plain `-` at the start or the end |
| `!`, `^`                        | negation, only right after `[`                                          |
| `\`                             | escapes the next character                                              |
| `/`                             | not allowed (compile error)                                             |
| `*` `?` `{` `}` `(` `)` `@` `+` | plain characters                                                        |

| Pattern  | Path | Match? |
| -------- | ---- | ------ |
| `[?*!(]` | `?`  | ✅     |
| `[?*!(]` | `*`  | ✅     |
| `[?*!(]` | `a`  | ❌     |
| `[-abc]` | `-`  | ✅     |
| `[abc-]` | `-`  | ✅     |
| `[a-]`   | `-`  | ✅     |
| `[!abc]` | `!`  | ✅     |

### Escapes inside a class

A `\` makes the next character a plain member, so `\-` is a dash and not a range, and `\!` is a member and not negation.

| Pattern   | Path | Match? |
| --------- | ---- | ------ |
| `[a\-z]`  | `-`  | ✅     |
| `[a\-z]`  | `b`  | ❌     |
| `[\!abc]` | `!`  | ✅     |
| `[\!abc]` | `d`  | ❌     |
| `[\?]`    | `?`  | ✅     |
| `[\]]`    | `]`  | ✅     |

Be careful with `[\]`: the `\` escapes the `]`, so the class never closes and you get `UnterminatedClass`.

Spec: [GLOB_SPEC §6.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## A `]` right after `[`, and empty classes

A `]` that comes first in a class (right after `[`, `[!`, or `[^`) is a plain member, not the closer. This is the POSIX rule. It lets you match `]` without an escape, and it means there is no such thing as an empty class.

| Pattern      | Result                                                                       |
| ------------ | ---------------------------------------------------------------------------- |
| `[]]`        | matches `]`                                                                  |
| `[!]]`       | matches any one character except `]`                                         |
| `[]-]`       | matches `]` or `-`                                                           |
| `[]`         | `UnterminatedClass` error: the `]` is a member, and nothing closes the class |
| `[!]`, `[^]` | `UnterminatedClass` error, for the same reason                               |
| `[`          | `UnterminatedClass` error                                                    |

The same rule is why `[!]foo` fails to compile. Write `[\!]foo` or `\!foo` instead.

Spec: [GLOB_SPEC §6.5](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Classes and braces

You can put a class inside a brace branch, and both work as usual.

| Pattern         | Path      | Match? |
| --------------- | --------- | ------ |
| `{[0-9],x}.log` | `1.log`   | ✅     |
| `{[0-9],x}.log` | `x.log`   | ✅     |
| `{[0-9],x}.log` | `10.log`  | ❌     |
| `*.[jt]s{,x}`   | `app.tsx` | ✅     |

Inside a class, `{`, `}`, and `,` are plain characters, so `[{]}` matches the path `{}`. Whichever bracket opens first owns the text until its own closer. In `{[}]`, the class swallows the `}`, the brace is never closed, and the pattern fails with `UnterminatedBrace`. See [Compile-time errors](./errors.md).

Spec: [GLOB_SPEC §7.9, §9.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Classes match one byte

::: warning Non-ASCII characters in a class
A class matches one **byte**, not one visible character. A character like `é` takes two bytes in UTF-8, so `[é]` does not match the path `é`. Use a brace instead: `{é,e}` matches either one.
:::

## POSIX named classes

Named classes such as `[[:alpha:]]` and `[[:digit:]]` are not supported and not recognized. `[[:alpha:]]` is the class `[[:alpha:]` followed by a literal `]`, so it matches `a]` and never `a` alone. Write the ranges yourself:

| Instead of    | Write         |
| ------------- | ------------- |
| `[[:digit:]]` | `[0-9]`       |
| `[[:alpha:]]` | `[a-zA-Z]`    |
| `[[:alnum:]]` | `[a-zA-Z0-9]` |

Spec: [GLOB_SPEC §6.6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
