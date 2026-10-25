# Differences from other tools

This page compares globstar with the tools the spec measures against: Bash, picomatch, globset (Rust), fast-glob (the npm walker and the Rust crate, which differ), and doublestar (Go). If you move patterns from one of them to globstar, check the rows below first.

::: tip What the other columns mean
Every cell for another tool was measured on 2026-10-16, the same runs as [GLOB_SPEC Appendix A](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md): Bash 5.2.37, picomatch 4.0.7, globset 0.4.20, fast-glob 3.3.3 from npm (it matches through micromatch and picomatch 2), the fast-glob crate 1.1.2 and doublestar v4.10.3. Windows cells come from a simulated platform, and tools change between versions. The globstar column is checked against `@globstar/core`, and each section shows the examples. The comparison does not include minimatch or gitignore. For every case the libraries' own test suites assert, and where globstar answers differently, see [`fixtures/ecosystem/`](https://github.com/shulaoda/globstar/blob/main/fixtures/ecosystem/README.md).
:::

`partial` is conditional, and the cell says how; `n/a` does not apply to that tool. ¹ Measured on Unix only.

## Options

| Behavior         | globstar                            | Bash               | picomatch       | globset                    | fast-glob (npm)             | fast-glob (crate)   | doublestar                                     |
| ---------------- | ----------------------------------- | ------------------ | --------------- | -------------------------- | --------------------------- | ------------------- | ---------------------------------------------- |
| `dot` default    | `true` (matcher) / `false` (walker) | `false`            | `false`         | `true` (no concept)        | `false`                     | `true` (no concept) | `true` (`Glob`: `WithNoHidden()`)              |
| Case-insensitive | ASCII only, default `false`         | `shopt nocaseglob` | `nocase` option | `case_insensitive` builder | `caseSensitiveMatch: false` | no                  | `WithCaseInsensitive()` (`Glob` only), Unicode |

In globstar:

| Pattern    | Path       | Options                 | Match? |
| ---------- | ---------- | ----------------------- | ------ |
| `*`        | `.env`     |                         | yes    |
| `*`        | `.env`     | `dot: false`            | no     |
| `*.ts`     | `MAIN.TS`  | `caseInsensitive: true` | yes    |
| `café.txt` | `CAFÉ.txt` | `caseInsensitive: true` | no     |

The matcher defaults to `dot: true`, and the walkers default to `dot: false`. Case folding covers ASCII letters only. See [How a path is matched](./matching.md#options).

## The `**` wildcard

| Behavior                               | globstar        | Bash                 | picomatch                     | globset                        | fast-glob (npm)                    | fast-glob (crate) | doublestar               |
| -------------------------------------- | --------------- | -------------------- | ----------------------------- | ------------------------------ | ---------------------------------- | ----------------- | ------------------------ |
| `**` must own a segment                | enforced        | yes                  | yes                           | yes (with `literal_separator`) | yes                                | yes               | yes                      |
| `a/**` matches `a`                     | no              | partial (lists `a/`) | yes (no with `strictSlashes`) | no                             | partial (walker: no; matcher: yes) | no                | yes                      |
| `**` in braces judged on the expansion | yes             | yes (expands first)  | no (branch-local `.*`)        | no                             | yes (expands first)                | no                | partial (not `**{/a,b}`) |
| `**` in a single-branch brace `{**}`   | degrades to `*` | degrades to `*`      | keeps globstar                | a group; `**` acts as `*`      | keeps globstar                     | a group (= `**`)  | a group (= `**`)         |
| `**/X` matches the absolute `/a/X`     | yes             | n/a                  | yes                           | yes                            | n/a (walker)                       | yes               | yes                      |

In globstar:

| Pattern       | Path           | Match? | Why                                                   |
| ------------- | -------------- | ------ | ----------------------------------------------------- |
| `src/a**b.ts` | `src/ax/yb.ts` | no     | `**` is not a whole name here, so it acts as `*`      |
| `a/**`        | `a`            | no     | `a/**` needs the `a/` prefix                          |
| `a/**`        | `a/`           | yes    | `a/` plus nothing                                     |
| `a{**,x}b`    | `ay/zb`        | no     | the expansion `a**b` has no whole-name `**`           |
| `{**,x}/b`    | `b`            | yes    | the expansion `**/b` keeps a real `**`                |
| `{**}`        | `{a}`          | yes    | one branch, so the braces are literal and `**` is `*` |
| `{**}`        | `{a/b}`        | no     | same reason: `*` does not cross `/`                   |
| `**/*.ts`     | `/a/b.ts`      | yes    | a leading `**/` also accepts absolute paths           |

A brace means "any one of these". globstar judges each `**` as if the brace were already expanded, so the text around it in the expansion decides whether it is a real `**`. To match a directory and everything below it, write `{a,a/**}`. See [The `**` wildcard](./globstar.md) and [Brace expansion](./braces.md).

## Character classes

| Behavior                | globstar          | Bash    | picomatch                          | globset     | fast-glob (npm) | fast-glob (crate) | doublestar                            |
| ----------------------- | ----------------- | ------- | ---------------------------------- | ----------- | --------------- | ----------------- | ------------------------------------- |
| POSIX character classes | no                | yes     | yes                                | no          | yes             | no                | no                                    |
| `[/]` inside a class    | parse error       | literal | partial (`/` or the literal text)  | matches `/` | never matches   | never matches     | partial (`Match`: `/`; `Glob`: error) |
| POSIX first-`]` rule    | yes               | yes     | yes                                | yes         | yes             | yes               | no (error)                            |
| `[!a]` same as `[^a]`   | yes (both negate) | yes     | no (only `[^a]`; yes with `posix`) | yes         | yes             | yes               | yes                                   |

In globstar:

| Pattern       | Path   | Match?                     |
| ------------- | ------ | -------------------------- |
| `[[:alpha:]]` | `a`    | no                         |
| `[/]`         |        | error: `UnterminatedClass` |
| `[]a]`        | `]`    | yes                        |
| `[!a].ts`     | `b.ts` | yes                        |
| `[!a].ts`     | `a.ts` | no                         |

A class can never hold `/`, so `[/]` fails to compile instead of silently matching nothing. A `]` right after `[`, `[!` or `[^` is a member, not the end. See [Character classes](./character-classes.md).

## Braces

| Behavior               | globstar | Bash | picomatch | globset | fast-glob (npm) | fast-glob (crate) | doublestar |
| ---------------------- | -------- | ---- | --------- | ------- | --------------- | ----------------- | ---------- |
| Brace ranges `{1..10}` | no       | yes  | yes       | no      | yes             | no                | no         |
| Brace nesting          | ≤ 32     | yes  | yes       | yes     | yes             | ≤ 10              | yes        |

In globstar:

| Pattern           | Path             | Match? |
| ----------------- | ---------------- | ------ |
| `file{1..3}.txt`  | `file2.txt`      | no     |
| `file{1..3}.txt`  | `file{1..3}.txt` | yes    |
| `file{1,2,3}.txt` | `file2.txt`      | yes    |

There are no ranges. `{1..3}` has no comma, so it is a one-branch brace and stays literal text. List the values instead. Nesting deeper than 32 levels is a `BraceNestingTooDeep` error, see [Errors and limits](./errors.md).

## Other syntax

| Behavior                 | globstar     | Bash          | picomatch | globset         | fast-glob (npm)         | fast-glob (crate)              | doublestar                  |
| ------------------------ | ------------ | ------------- | --------- | --------------- | ----------------------- | ------------------------------ | --------------------------- |
| Extglobs                 | no (literal) | yes (`shopt`) | yes       | no              | yes                     | no                             | no                          |
| Leading-`!` negation     | yes          | n/a           | yes       | no (literal)    | yes (an ignore pattern) | yes                            | no (literal)                |
| `/` required in patterns | yes          | n/a           | yes       | partial         | yes                     | yes                            | partial (`PathMatch`)       |
| `\` is always the escape | yes          | yes           | yes       | partial         | yes                     | yes ¹                          | partial (`PathMatch`)       |
| Stray `]`, `}`, `)`      | literal      | literal       | literal   | `}` is an error | literal                 | literal                        | partial (`}`: `Glob` error) |
| Unclosed constructs      | parse error  | literal       | partial   | error           | partial (no error)      | no match (`validate()` errors) | error                       |

In globstar:

| Pattern       | Path          | Match?                     |
| ------------- | ------------- | -------------------------- |
| `@(a\|b).ts`  | `a.ts`        | no                         |
| `@(a\|b).ts`  | `@(a\|b).ts`  | yes                        |
| `!*.test.ts`  | `a.ts`        | yes                        |
| `src\main.ts` | `src/main.ts` | no                         |
| `\*.md`       | `*.md`        | yes                        |
| `\*.md`       | `a.md`        | no                         |
| `a]b`         | `a]b`         | yes                        |
| `file(1).txt` | `file(1).txt` | yes                        |
| `{a,b`        |               | error: `UnterminatedBrace` |
| `[abc`        |               | error: `UnterminatedClass` |

Extglob syntax such as `@(a|b)` is plain text. In a pattern, `/` is the only separator and `\` always escapes the next character, so `src\main.ts` means `srcmain.ts`. An unclosed `[` or `{` is a hard error, never a silent fallback to literal text. See [Pattern syntax](./syntax.md).

## Paths and directories

| Behavior                                 | globstar             | Bash | picomatch                  | globset  | fast-glob (npm) | fast-glob (crate) | doublestar             |
| ---------------------------------------- | -------------------- | ---- | -------------------------- | -------- | --------------- | ----------------- | ---------------------- |
| `\` in a path is a separator             | platform-conditional | no   | partial (`windows` option) | platform | n/a (walker)    | platform ¹        | platform (`PathMatch`) |
| `match_dir` API                          | yes (first-class)    | n/a  | no                         | no       | no              | no                | no                     |
| `*` and `**` match `.` and `..` (dot on) | yes                  | no   | no                         | yes      | no              | yes               | yes                    |

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
