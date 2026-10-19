# How a path is matched

This page explains what "match" means in globstar: the whole path must fit the pattern, how `/` and `\` are treated, what the two options `dot` and `caseInsensitive` change, and the exact rules for names that start with a dot. Everything here holds for both the Rust crate `globstar` and the JS package `@globstar/core`.

## The whole path must match

A pattern matches a path only when the pattern covers every byte of the path, from the first to the last. There is no "contains" or substring search, as in `grep`. Since `*` never crosses a `/`, `*.ts` only matches a path that has no `/` at all.

| Pattern        | Path             | Match? |
| -------------- | ---------------- | ------ |
| `src/*.ts`     | `src/main.ts`    | ✅     |
| `*.ts`         | `src/main.ts`    | ❌     |
| `main`         | `src/main.ts`    | ❌     |
| `README.md`    | `docs/README.md` | ❌     |
| `**/README.md` | `docs/README.md` | ✅     |

To match a name at any depth, start the pattern with `**/`. See [The `**` wildcard](./globstar.md).

Spec: [GLOB_SPEC §16.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Paths are just strings

The matcher never looks at the file system. It does not resolve `.` or `..`, follow symlinks, or add the current directory. `./src/a.ts` and `src/a.ts` are different strings, so they can give different answers.

| Pattern      | Path          | Options      | Match? |
| ------------ | ------------- | ------------ | ------ |
| `src/a.ts`   | `./src/a.ts`  |              | ❌     |
| `src/*/a.ts` | `src/../a.ts` |              | ✅     |
| `src/*/a.ts` | `src/../a.ts` | `dot: false` | ❌     |

::: warning `*` can match `..`
With the default `dot: true`, `*` matches the names `.` and `..` like any other name. So `src/*/a.ts` matches `src/../a.ts`, a file that is not inside `src` at all. If paths come from users, clean them up (for example with `path.normalize`) before you match.
:::

Spec: [GLOB_SPEC §14.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Path separators

### In patterns, only `/`

Inside a pattern, `/` is the only separator, on every platform. `\` is always the escape character, never a separator. So the pattern `src\main.ts` does not match `src/main.ts`: `\m` is just a literal `m`, and the pattern matches the path `srcmain.ts`.

If your tool takes patterns written with Windows `\` separators, change them to `/` before you compile them. An escaped slash (`\/`) is a compile error, see [Errors](./errors.md).

Spec: [GLOB_SPEC §2.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### In paths, `/` always and `\` on Windows

A path may use `/` on every platform. On Windows, `\` in a path also counts as a separator. Rust decides this with `std::path::is_separator`. JS treats `\` as a separator when `process.platform` is `"win32"`; where there is no `process` (for example in a browser), only `/` counts.

| Pattern       | Path           | macOS / Linux | Windows |
| ------------- | -------------- | ------------- | ------- |
| `src/*.ts`    | `src\main.ts`  | ❌            | ✅      |
| `src/**/*.ts` | `src\lib\a.ts` | ❌            | ✅      |
| `*.ts`        | `src\main.ts`  | ✅            | ❌      |

The last row shows the cost: on macOS and Linux, `\` is an ordinary byte in a file name, so `*` can match it. The matcher does not rewrite `\` to `/`; it only treats it as a separator. If you need the same answer on every platform, convert paths to `/` before matching.

Spec: [GLOB_SPEC §2.2, §14.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### Repeated separators

Each `/` in a pattern matches exactly one separator in the path. Paths with `//` are not cleaned up for you.

| Pattern       | Path           | Match? |
| ------------- | -------------- | ------ |
| `src/*.ts`    | `src//main.ts` | ❌     |
| `a//b`        | `a//b`         | ✅     |
| `a//b`        | `a/b`          | ❌     |
| `src/**/*.ts` | `src//main.ts` | ✅     |

There are two exceptions, both about `**`. A `/` next to `**` (as in `/**/`) accepts one or more separators. A pattern that starts with `**/` also accepts any separators at the start of the path, so it matches absolute and UNC paths too:

| Pattern        | Path                       | Match? |
| -------------- | -------------------------- | ------ |
| `**/*.ts`      | `/home/me/src/a.ts`        | ✅     |
| `**/README.md` | `//server/share/README.md` | ✅     |
| `src/**/*.ts`  | `/repo/src/a.ts`           | ❌     |

Spec: [GLOB_SPEC §14.2, §9.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### Trailing separators

A `/` at the end of a path is a real byte, and the pattern must match it like any other.

| Pattern  | Path   | Match? |
| -------- | ------ | ------ |
| `src`    | `src/` | ❌     |
| `src/`   | `src/` | ✅     |
| `src/`   | `src`  | ❌     |
| `src/**` | `src/` | ✅     |
| `src/**` | `src`  | ❌     |

`src/**` needs the `src/` prefix, so it does not match `src` itself. File systems usually return directory paths without a trailing `/`. To ask whether a directory may hold matches, see [Directory matching](./directory-matching.md).

Spec: [GLOB_SPEC §9.2, §14.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Options

There are exactly two options. Every syntax feature (braces, `**`, classes, `!`) is always on.

| Option           | JS                | Rust               | Default | What it changes                              |
| ---------------- | ----------------- | ------------------ | ------- | -------------------------------------------- |
| dot              | `dot`             | `dot`              | `true`  | May wildcards match a leading `.` in a name? |
| case-insensitive | `caseInsensitive` | `case_insensitive` | `false` | Fold ASCII `A-Z` / `a-z` when comparing      |

```js
import { compileMatcher } from "@globstar/core";

const m = compileMatcher("src/**/*.ts", { dot: false, caseInsensitive: true });
m.match("SRC/lib/a.TS"); // true
```

```rust
use globstar::{CompileOptions, Glob};

let opts = CompileOptions::default().dot(false).case_insensitive(true);
let glob = Glob::new_with("src/**/*.ts", opts)?;
```

### `dot`

When `dot` is `false`, a name that starts with `.` is hidden from wildcards. You must write the dot yourself to match it. The exact rules are in [Dot protection, precisely](#dot-protection-precisely).

The matcher default is `dot: true`. The matcher only compares strings, and "hidden files" is a file system habit, not part of the pattern language. The file system walkers (`@globstar/walk`, `globstar-walk`) default to `dot: false`, like bash and picomatch.

| Pattern       | Path              | Options      | Match? |
| ------------- | ----------------- | ------------ | ------ |
| `*`           | `.env`            |              | ✅     |
| `*`           | `.env`            | `dot: false` | ❌     |
| `.*`          | `.env`            | `dot: false` | ✅     |
| `*.txt`       | `.txt`            | `dot: false` | ❌     |
| `**/*`        | `a/.b`            | `dot: false` | ❌     |
| `src/**/*.ts` | `src/.cache/a.ts` | `dot: false` | ❌     |

Spec: [GLOB_SPEC §3.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### `caseInsensitive`

When `caseInsensitive` (Rust: `case_insensitive`) is `true`, ASCII letters match their other case. Only `A-Z` and `a-z` fold. Other bytes, such as `É` and `é`, must be equal. The default is `false` for the matcher and the walkers.

| Pattern     | Path        | Options                 | Match? |
| ----------- | ----------- | ----------------------- | ------ |
| `*.ts`      | `MAIN.TS`   |                         | ❌     |
| `*.ts`      | `MAIN.TS`   | `caseInsensitive: true` | ✅     |
| `README.md` | `readme.md` | `caseInsensitive: true` | ✅     |
| `[A-Z]*.md` | `readme.md` | `caseInsensitive: true` | ✅     |
| `café.txt`  | `CAFÉ.txt`  | `caseInsensitive: true` | ❌     |
| `café.txt`  | `CAFé.txt`  | `caseInsensitive: true` | ✅     |

The option changes only how bytes are compared, never how the pattern is read. `[a-A]` is still an `InvalidRange` error with the option on, because `a` comes after `A` in byte order. If you need full Unicode case folding, normalize both the pattern and the path yourself (for example, lowercase both) before you match.

Spec: [GLOB_SPEC §13](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### Options that do not exist

There is no switch to turn off braces, to turn off `**`, or to fold Unicode case. Each switch would add state to the parser for very few users, and escapes already cover the need: write `\{a,b\}` to match a literal `{a,b}`. For Unicode case, normalize the strings first.

Spec: [GLOB_SPEC §3.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Dot protection, precisely

With `dot: false`, a `.` in the path is protected when it is the first byte of the path, or when the byte before it is a separator. In other words: a dot at the start of a name. A dot anywhere else is an ordinary byte.

| In the pattern                                  | Can it match a protected `.`?                       |
| ----------------------------------------------- | --------------------------------------------------- |
| `*`                                             | No                                                  |
| `?`                                             | No                                                  |
| `[^x]` or `[!x]`                                | No                                                  |
| a literal `.` (or `\.`)                         | Yes                                                 |
| a class that lists `.`, such as `[.]`, `[a-z.]` | Yes                                                 |
| `**`                                            | Not itself; each name it passes over obeys the rule |

| Pattern       | Path              | Options      | Match? |
| ------------- | ----------------- | ------------ | ------ |
| `?env`        | `.env`            | `dot: false` | ❌     |
| `[!x]env`     | `.env`            | `dot: false` | ❌     |
| `[.]env`      | `.env`            | `dot: false` | ✅     |
| `src/**`      | `src/.git/config` | `dot: false` | ❌     |
| `src/.git/**` | `src/.git/config` | `dot: false` | ✅     |
| `**/.env`     | `app/.env`        | `dot: false` | ✅     |
| `file?txt`    | `file.txt`        | `dot: false` | ✅     |
| `*`           | `..`              | `dot: false` | ❌     |
| `.*`          | `..`              | `dot: false` | ✅     |

`file?txt` matches because that `.` is not at the start of a name. On Windows a dot right after `\` is protected too, since `\` is a separator there. The names `.` and `..` are plain bytes: with `dot: false`, `*` matches neither, but `.*` matches both, because the literal `.` takes the first dot and `*` may take the rest.

Spec: [GLOB_SPEC §12](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Multi-byte characters

Matching works on bytes. A pattern and a JS path are compared as UTF-8 bytes; a Rust path is a `&[u8]`, so even names that are not valid UTF-8 work. `?` matches exactly one byte, not one character. A non-ASCII character takes two to four bytes, so one `?` cannot match it. `*` has no such problem.

| Pattern  | Path      | Match? |
| -------- | --------- | ------ |
| `?.txt`  | `é.txt`   | ❌     |
| `??.txt` | `é.txt`   | ✅     |
| `?.md`   | `日.md`   | ❌     |
| `???.md` | `日.md`   | ✅     |
| `*.md`   | `日本.md` | ✅     |

Spec: [GLOB_SPEC §14.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Leading `!` and the options

A leading `!` flips the result of the rest of the pattern. It is always on; no option turns it off. The options apply to the rest of the pattern first, and then `!` flips that answer. To match a name that really starts with `!`, escape it: `\!important.md`.

| Pattern      | Path        | Options                 | Match? |
| ------------ | ----------- | ----------------------- | ------ |
| `!*.test.ts` | `a.test.ts` |                         | ❌     |
| `!*.test.ts` | `a.ts`      |                         | ✅     |
| `!*`         | `.env`      |                         | ❌     |
| `!*`         | `.env`      | `dot: false`            | ✅     |
| `!*.TS`      | `a.ts`      | `caseInsensitive: true` | ❌     |

::: warning `dot: false` makes `!` patterns match dotfiles
With `dot: false`, `*` cannot match `.env`, so `!*` does match it. A `!` pattern flips the hidden names too. For how `!` works inside a list of patterns, see [Multiple patterns](./multi-pattern.md).
:::

Spec: [GLOB_SPEC §11.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
