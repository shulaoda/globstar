# Differences from other tools

This page answers "how is globstar different from the glob tool I already use?". It covers the tools the spec compares against: Bash, picomatch, globset (Rust), fast-glob (the npm walker and the Rust crate, which differ), and doublestar (Go). If you move patterns from one of them to globstar, check the rows below first.

::: tip What the other columns mean
Every cell for another tool was measured on 2026-10-16, the same runs as [GLOB_SPEC Appendix A](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md): Bash 5.2.37, picomatch 4.0.7, globset 0.4.20, fast-glob 3.3.3 from npm (it matches through micromatch and picomatch 2), the fast-glob crate 1.1.2 and doublestar v4.10.3. Windows cells come from a simulated platform, and tools change between versions. The globstar column is checked against `@globstar/core`, and each section shows the examples. The comparison does not include minimatch or gitignore. For every case the libraries' own test suites assert, and where globstar answers differently, see [`fixtures/ecosystem/`](https://github.com/shulaoda/globstar/blob/main/fixtures/ecosystem/README.md).
:::

Symbols: ✅ yes, ❌ no, ⚠️ partial or conditional (the cell says how), n/a does not apply to that tool. ¹ Measured on Unix only.

## Options

| Behavior         | globstar                            | Bash               | picomatch       | globset                    | fast-glob (npm)             | fast-glob (crate)   | doublestar                                     |
| ---------------- | ----------------------------------- | ------------------ | --------------- | -------------------------- | --------------------------- | ------------------- | ---------------------------------------------- |
| `dot` default    | `true` (matcher) / `false` (walker) | `false`            | `false`         | `true` (no concept)        | `false`                     | `true` (no concept) | `true` (`Glob`: `WithNoHidden()`)              |
| Case-insensitive | ASCII only, default `false`         | `shopt nocaseglob` | `nocase` option | `case_insensitive` builder | `caseSensitiveMatch: false` | ❌                  | `WithCaseInsensitive()` (`Glob` only), Unicode |

In globstar:

| Pattern    | Path       | Options                 | Match? |
| ---------- | ---------- | ----------------------- | ------ |
| `*`        | `.env`     |                         | ✅     |
| `*`        | `.env`     | `dot: false`            | ❌     |
| `*.ts`     | `MAIN.TS`  | `caseInsensitive: true` | ✅     |
| `café.txt` | `CAFÉ.txt` | `caseInsensitive: true` | ❌     |

The matcher defaults to `dot: true`, and the walkers default to `dot: false`. Case folding covers ASCII letters only. See [How a path is matched](./matching.md#options).

## The `**` wildcard

| Behavior                               | globstar        | Bash               | picomatch                    | globset                       | fast-glob (npm)               | fast-glob (crate) | doublestar          |
| -------------------------------------- | --------------- | ------------------ | ---------------------------- | ----------------------------- | ----------------------------- | ----------------- | ------------------- |
| `**` must own a segment                | enforced        | ✅                 | ✅                           | ✅ (with `literal_separator`) | ✅                            | ✅                | ✅                  |
| `a/**` matches `a`                     | ❌              | ⚠️ (lists `a/`)    | ✅ (❌ with `strictSlashes`) | ❌                            | ⚠️ (walker: no; matcher: yes) | ❌                | ✅                  |
| `**` in braces judged on the expansion | ✅              | ✅ (expands first) | ❌ (branch-local `.*`)       | ❌                            | ✅ (expands first)            | ❌                | ⚠️ (not `**{/a,b}`) |
| `**` in a single-branch brace `{**}`   | degrades to `*` | degrades to `*`    | keeps globstar               | a group; `**` acts as `*`     | keeps globstar                | a group (= `**`)  | a group (= `**`)    |
| `**/X` matches the absolute `/a/X`     | ✅              | n/a                | ✅                           | ✅                            | n/a (walker)                  | ✅                | ✅                  |

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

| Behavior                | globstar         | Bash    | picomatch                         | globset     | fast-glob (npm) | fast-glob (crate) | doublestar                       |
| ----------------------- | ---------------- | ------- | --------------------------------- | ----------- | --------------- | ----------------- | -------------------------------- |
| POSIX character classes | ❌               | ✅      | ✅                                | ❌          | ✅              | ❌                | ❌                               |
| `[/]` inside a class    | parse error      | literal | ⚠️ (`/` or the literal text)      | matches `/` | never matches   | never matches     | ⚠️ (`Match`: `/`; `Glob`: error) |
| POSIX first-`]` rule    | ✅               | ✅      | ✅                                | ✅          | ✅              | ✅                | ❌ (error)                       |
| `[!a]` same as `[^a]`   | ✅ (both negate) | ✅      | ❌ (only `[^a]`; ✅ with `posix`) | ✅          | ✅              | ✅                | ✅                               |

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

| Behavior               | globstar | Bash | picomatch | globset | fast-glob (npm) | fast-glob (crate) | doublestar |
| ---------------------- | -------- | ---- | --------- | ------- | --------------- | ----------------- | ---------- |
| Brace ranges `{1..10}` | ❌       | ✅   | ✅        | ❌      | ✅              | ❌                | ❌         |
| Brace nesting          | ≤ 32     | ✅   | ✅        | ✅      | ✅              | ≤ 10              | ✅         |

In globstar:

| Pattern           | Path             | Match? |
| ----------------- | ---------------- | ------ |
| `file{1..3}.txt`  | `file2.txt`      | ❌     |
| `file{1..3}.txt`  | `file{1..3}.txt` | ✅     |
| `file{1,2,3}.txt` | `file2.txt`      | ✅     |

There are no ranges. `{1..3}` has no comma, so it is a one-branch brace and stays literal text. List the values instead. Nesting deeper than 32 levels is a `BraceNestingTooDeep` error, see [Errors and limits](./errors.md).

## Other syntax

| Behavior                 | globstar     | Bash         | picomatch | globset         | fast-glob (npm)        | fast-glob (crate)              | doublestar             |
| ------------------------ | ------------ | ------------ | --------- | --------------- | ---------------------- | ------------------------------ | ---------------------- |
| Extglobs                 | ❌ (literal) | ✅ (`shopt`) | ✅        | ❌              | ✅                     | ❌                             | ❌                     |
| Leading-`!` negation     | ✅           | n/a          | ✅        | ❌ (literal)    | ✅ (an ignore pattern) | ✅                             | ❌ (literal)           |
| `/` required in patterns | ✅           | n/a          | ✅        | ⚠️              | ✅                     | ✅                             | ⚠️ (`PathMatch`)       |
| `\` is always the escape | ✅           | ✅           | ✅        | ⚠️              | ✅                     | ✅ ¹                           | ⚠️ (`PathMatch`)       |
| Stray `]`, `}`, `)`      | literal      | literal      | literal   | `}` is an error | literal                | literal                        | ⚠️ (`}`: `Glob` error) |
| Unclosed constructs      | parse error  | literal      | ⚠️        | error           | ⚠️ (no error)          | no match (`validate()` errors) | error                  |

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

| Behavior                                 | globstar             | Bash | picomatch             | globset  | fast-glob (npm) | fast-glob (crate) | doublestar             |
| ---------------------------------------- | -------------------- | ---- | --------------------- | -------- | --------------- | ----------------- | ---------------------- |
| `\` in a path is a separator             | platform-conditional | ❌   | ⚠️ (`windows` option) | platform | n/a (walker)    | platform ¹        | platform (`PathMatch`) |
| `match_dir` API                          | ✅ (first-class)     | n/a  | ❌                    | ❌       | ❌              | ❌                | ❌                     |
| `*` and `**` match `.` and `..` (dot on) | ✅                   | ❌   | ❌                    | ✅       | ❌              | ✅                | ✅                     |

In globstar:

| Call                                               | Result                                      |
| -------------------------------------------------- | ------------------------------------------- |
| `compileMatcher('src/*.ts').match('src\\main.ts')` | `false` on macOS / Linux, `true` on Windows |
| `compileMatcher('src/**/*.ts').matchDir('src')`    | `DirMatch.Descend`                          |
| `compileMatcher('src/**/*.ts').matchDir('tests')`  | `DirMatch.Pruned`                           |

With `dot: true`, the matcher's default, `*` and `**` also match the names `.` and `..`, so `public/**` matches `public/../secret.txt`; picomatch never does. Normalize a path from outside before you match it. The walkers never see such names.

A `\` in a path counts as a separator only on Windows. `matchDir` tells a walker whether a directory can hold matches, so it can skip whole subtrees. See [How a path is matched](./matching.md#in-paths-always-and-on-windows) and [Directory matching](./directory-matching.md).

## Summary

Of these tools, picomatch is the closest. globstar is stricter about input (patterns must use `/`), treats directory matching as a first-class operation, and is stricter about syntax errors: an unclosed class or brace is always an error, never silently turned into literal text.

Spec: [GLOB_SPEC Appendix A](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
