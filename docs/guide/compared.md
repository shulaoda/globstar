# Differences from other tools

This page answers "how is globstar different from the glob tool I already use?". It covers the tools the spec compares against: Bash, picomatch, globset (Rust), fast-glob, and doublestar (Go). If you move patterns from one of them to globstar, check the rows below first.

::: tip What the other columns mean
The columns for other tools repeat what the spec states in [GLOB_SPEC §16](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md). They were not re-tested for this page, and tools change between versions. The globstar column is checked against `@globstar/core`, and each section shows the examples. The spec's comparison table does not include minimatch or gitignore, so this page does not either.
:::

Symbols: ✅ yes, ❌ no, ⚠️ the spec marks it as partial or conditional, n/a does not apply to that tool.

## Options

| Behavior         | globstar                            | Bash               | picomatch       | globset                    | fast-glob                  | doublestar    |
| ---------------- | ----------------------------------- | ------------------ | --------------- | -------------------------- | -------------------------- | ------------- |
| `dot` default    | `true` (matcher) / `false` (walker) | `false`            | `false`         | `true` (no concept)        | `true` (no concept)        | `false`       |
| Case-insensitive | ASCII-only, default `false`         | `shopt nocaseglob` | `nocase` option | `case_insensitive` builder | `caseSensitiveMatch=false` | Unicode-aware |

In globstar:

| Pattern    | Path       | Options                 | Match? |
| ---------- | ---------- | ----------------------- | ------ |
| `*`        | `.env`     |                         | ✅     |
| `*`        | `.env`     | `dot: false`            | ❌     |
| `*.ts`     | `MAIN.TS`  | `caseInsensitive: true` | ✅     |
| `café.txt` | `CAFÉ.txt` | `caseInsensitive: true` | ❌     |

The matcher defaults to `dot: true`, and the walkers default to `dot: false`. Case folding covers ASCII letters only. See [How a path is matched](./matching.md#options).

## The `**` wildcard

| Behavior                                | globstar        | Bash               | picomatch              | globset                                | fast-glob      | doublestar |
| --------------------------------------- | --------------- | ------------------ | ---------------------- | -------------------------------------- | -------------- | ---------- |
| `**` must own a segment                 | enforced        | ✅                 | ✅                     | ✅                                     | ✅             | ✅         |
| `a/**` matches `a`                      | ❌              | ⚠️                 | ⚠️                     | ❌                                     | ❌             | ❌         |
| `**` in braces judged on the expansion  | ✅              | ✅ (expands first) | ❌ (branch-local `.*`) | ❌ (demotes to `*` even at boundaries) | ❌             | n/a        |
| `**` in a single-branch brace `{**}`    | degrades to `*` | literal            | keeps globstar         | ⚠️                                     | keeps globstar | ⚠️         |
| `**/X` matches the absolute path `/a/X` | ✅              | n/a                | ✅                     | ✅                                     | ✅             | ❌         |

In globstar:

| Pattern       | Path           | Match? | Why                                                   |
| ------------- | -------------- | ------ | ----------------------------------------------------- |
| `src/a**b.ts` | `src/ax/yb.ts` | ❌     | `**` is not a whole name here, so it acts as `*`      |
| `a/**`        | `a`            | ❌     | `a/**` needs the `a/` prefix                          |
| `a/**`        | `a/`           | ✅     | `a/` plus nothing                                     |
| `a{**,x}b`    | `ay/zb`        | ❌     | the expansion `a**b` has no whole-name `**`           |
| `{**,x}/b`    | `b`            | ✅     | the expansion `**/b` keeps its `**` power             |
| `{**}`        | `{a}`          | ✅     | one branch, so the braces are literal and `**` is `*` |
| `{**}`        | `{a/b}`        | ❌     | same reason: `*` does not cross `/`                   |
| `**/*.ts`     | `/a/b.ts`      | ✅     | a leading `**/` also accepts absolute paths           |

A brace means "any one of these". globstar judges each `**` as if the brace were already expanded, so the text around it in the expansion decides whether it is a real `**`. To match a directory and everything below it, write `{a,a/**}`. See [The `**` wildcard](./globstar.md) and [Brace expansion](./braces.md).

## Character classes

| Behavior                            | globstar         | Bash    | picomatch        | globset | fast-glob        | doublestar |
| ----------------------------------- | ---------------- | ------- | ---------------- | ------- | ---------------- | ---------- |
| POSIX character classes             | ❌               | ✅      | ✅               | ❌      | ❌               | ⚠️         |
| `[/]` inside a class                | parse error      | literal | literal          | literal | literal          | ⚠️         |
| POSIX first-`]` rule                | ✅               | ✅      | ✅               | ❌      | ❌               | ⚠️         |
| Negated class `[!a]` same as `[^a]` | ✅ (both negate) | ✅      | ❌ (only `[^a]`) | ⚠️      | ❌ (only `[^a]`) | ⚠️         |

In globstar:

| Pattern       | Path   | Match?                     |
| ------------- | ------ | -------------------------- |
| `[[:alpha:]]` | `a`    | ❌                         |
| `[/]`         |        | error: `UnterminatedClass` |
| `[]a]`        | `]`    | ✅                         |
| `[!a].ts`     | `b.ts` | ✅                         |
| `[!a].ts`     | `a.ts` | ❌                         |

A class can never hold `/`, so `[/]` fails to compile instead of silently matching nothing. A `]` right after `[`, `[!` or `[^` is a member, not the end. See [Character classes](./character-classes.md).

## Braces

| Behavior        | globstar | Bash | picomatch | globset | fast-glob | doublestar |
| --------------- | -------- | ---- | --------- | ------- | --------- | ---------- |
| Brace `{1..10}` | ❌       | ✅   | ✅        | ❌      | ❌        | ❌         |
| Brace nesting   | ≤ 32     | ✅   | ✅        | ✅      | ≤ 10      | ✅         |

In globstar:

| Pattern           | Path             | Match? |
| ----------------- | ---------------- | ------ |
| `file{1..3}.txt`  | `file2.txt`      | ❌     |
| `file{1..3}.txt`  | `file{1..3}.txt` | ✅     |
| `file{1,2,3}.txt` | `file2.txt`      | ✅     |

There are no ranges. `{1..3}` has no comma, so it is a one-branch brace and stays literal text. List the values instead. Nesting deeper than 32 levels is a `BraceNestingTooDeep` error, see [Errors and limits](./errors.md).

## Other syntax

| Behavior                 | globstar     | Bash         | picomatch | globset | fast-glob | doublestar |
| ------------------------ | ------------ | ------------ | --------- | ------- | --------- | ---------- |
| Extglobs                 | ❌ (literal) | ✅ (`shopt`) | ✅        | ❌      | ❌        | ✅         |
| Leading-`!` negation     | ✅           | n/a          | ✅        | ⚠️      | ✅        | ✅         |
| `/` required in patterns | ✅           | n/a          | ⚠️        | ⚠️      | ⚠️        | ⚠️         |
| `\` is always the escape | ✅           | ✅           | ✅        | ⚠️      | ⚠️        | ⚠️         |
| Stray `]`, `}`, `)`      | literal      | literal      | literal   | literal | literal   | literal    |
| Unclosed constructs      | parse error  | literal      | ⚠️        | error   | error     | literal    |

In globstar:

| Pattern       | Path          | Match?                     |
| ------------- | ------------- | -------------------------- |
| `@(a\|b).ts`  | `a.ts`        | ❌                         |
| `@(a\|b).ts`  | `@(a\|b).ts`  | ✅                         |
| `!*.test.ts`  | `a.ts`        | ✅                         |
| `src\main.ts` | `src/main.ts` | ❌                         |
| `\*.md`       | `*.md`        | ✅                         |
| `\*.md`       | `a.md`        | ❌                         |
| `a]b`         | `a]b`         | ✅                         |
| `file(1).txt` | `file(1).txt` | ✅                         |
| `{a,b`        |               | error: `UnterminatedBrace` |
| `[abc`        |               | error: `UnterminatedClass` |

Extglob syntax such as `@(a|b)` is plain text. In a pattern, `/` is the only separator and `\` always escapes the next character, so `src\main.ts` means `srcmain.ts`. An unclosed `[` or `{` is a hard error, never a silent fallback to literal text. See [Pattern syntax](./syntax.md).

## Paths and directories

| Behavior                     | globstar             | Bash | picomatch     | globset  | fast-glob | doublestar |
| ---------------------------- | -------------------- | ---- | ------------- | -------- | --------- | ---------- |
| `\` in a path is a separator | platform-conditional | ❌   | ✅ (JS paths) | platform | ❌        | platform   |
| `match_dir` API              | ✅ (first-class)     | n/a  | ❌            | ✅       | ❌        | ❌         |

In globstar:

| Call                                               | Result                                      |
| -------------------------------------------------- | ------------------------------------------- |
| `compileMatcher('src/*.ts').match('src\\main.ts')` | `false` on macOS / Linux, `true` on Windows |
| `compileMatcher('src/**/*.ts').matchDir('src')`    | `DirMatch.Descend`                          |
| `compileMatcher('src/**/*.ts').matchDir('tests')`  | `DirMatch.Pruned`                           |

A `\` in a path counts as a separator only on Windows. `matchDir` tells a walker whether a directory can hold matches, so it can skip whole subtrees. See [How a path is matched](./matching.md#in-paths-always-and-on-windows) and [Directory matching](./directory-matching.md).

## Summary

The spec sums it up this way: globstar is closest to picomatch and doublestar. It is stricter about input (patterns must use `/`), it treats directory matching as a first-class operation, and it is stricter about syntax errors: an unclosed class or brace is always an error, never silently turned into literal text.

Spec: [GLOB_SPEC §16](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
