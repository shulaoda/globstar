# Brace expansion

A brace like `{js,ts}` lists alternatives: the pattern matches if any one of them fits. This page explains what a brace means, the forms you can write (nested braces, empty branches), when braces are plain text or an error, and the limits that keep compile time safe. For the rest of the syntax, see [Pattern syntax](./syntax.md).

All examples use the default options (`dot: true`, `caseInsensitive: false`).

## What a brace means

Take a pattern with a brace and replace the brace with each branch in turn. You get a list of plain patterns, called its **expansions**. The original pattern matches a path when **at least one** expansion matches it.

- `*.{js,ts}` expands to `*.js` and `*.ts`.
- `{src,test}/**/*.ts` expands to `src/**/*.ts` and `test/**/*.ts`.
- Two braces give every combination: `{a,b}{c,d}` expands to `ac`, `ad`, `bc`, and `bd`.

Each expansion is a complete pattern, and every rule on these pages applies to it on its own. That matters most for `**`: whether a `**` next to a brace is a real "any depth" wildcard is decided per expansion (see [Braces and `**`](#braces-and)).

globstar does not build the list of expansions in memory. A brace with thousands of branches, or many braces in a row, still compiles fast and small. The one exception is braces whose branches cross a `/`, like `{a,/}` written many times in a row: the number of different segments can grow with the square of their count, and such a pattern fails with `TooComplex` (see [Errors and limits](./errors.md)).

Spec: [GLOB_SPEC §10.1, §10.5](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Basic forms

| Form          | Meaning                                 |
| ------------- | --------------------------------------- |
| `{a,b,c}`     | `a`, `b`, or `c`                        |
| `{a*b,c?}`    | branches may hold wildcards             |
| `{a,{b,c}}`   | braces may nest                         |
| `{a,b}{c,d}`  | braces in a row: every combination      |
| `{src/a,lib}` | a branch may hold `/` and span segments |

| Pattern                 | Path           | Match? |
| ----------------------- | -------------- | ------ |
| `*.{js,ts}`             | `app.js`       | ✅     |
| `*.{js,ts}`             | `app.ts`       | ✅     |
| `*.{js,ts}`             | `app.css`      | ❌     |
| `*.{js,ts}`             | `src/app.js`   | ❌     |
| `{README,CHANGELOG}.md` | `CHANGELOG.md` | ✅     |
| `{README,CHANGELOG}.md` | `LICENSE.md`   | ❌     |
| `*.{spec,test}.{js,ts}` | `a.spec.js`    | ✅     |
| `*.{spec,test}.{js,ts}` | `a.test.ts`    | ✅     |
| `*.{spec,test}.{js,ts}` | `a.js`         | ❌     |
| `{src,test}/**/*.ts`    | `test/x.ts`    | ✅     |
| `{src,test}/**/*.ts`    | `docs/x.ts`    | ❌     |

The `src/app.js` row fails because each expansion is a normal pattern: `*.js` does not cross `/`.

Spec: [GLOB_SPEC §10.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Nesting

A branch can contain another brace. Nesting is handy when several paths share a prefix.

| Pattern                              | Path                        | Match? |
| ------------------------------------ | --------------------------- | ------ |
| `{src/{components,pages},lib}/*.tsx` | `src/components/Button.tsx` | ✅     |
| `{src/{components,pages},lib}/*.tsx` | `src/pages/Home.tsx`        | ✅     |
| `{src/{components,pages},lib}/*.tsx` | `lib/x.tsx`                 | ✅     |
| `{src/{components,pages},lib}/*.tsx` | `src/x.tsx`                 | ❌     |

## Empty branches

A branch can be empty. `{a,}` means "`a` or nothing". This is the easy way to make part of a name optional.

| Pattern                     | Path           | Match? |
| --------------------------- | -------------- | ------ |
| `app{,.min}.js`             | `app.js`       | ✅     |
| `app{,.min}.js`             | `app.min.js`   | ✅     |
| `.env{,.local,.production}` | `.env`         | ✅     |
| `.env{,.local,.production}` | `.env.local`   | ✅     |
| `.env{,.local,.production}` | `.env.test`    | ❌     |
| `{,src/}index.ts`           | `index.ts`     | ✅     |
| `{,src/}index.ts`           | `src/index.ts` | ✅     |

The empty pattern `""` is a compile error, but an empty branch is fine: it just adds "nothing" as one of the choices.

Spec: [GLOB_SPEC §10.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## A brace needs a comma

A brace with no comma is not a brace. `{a}` is just the three characters `{`, `a`, `}`. This follows Bash, picomatch and the npm fast-glob; globset, doublestar and the Rust fast-glob crate read `{a}` as a group, so there it matches `a`. It also means range forms like `{1..3}` are plain text, because ranges are not supported.

| Pattern      | Path         | Match? |
| ------------ | ------------ | ------ |
| `{src}/*.ts` | `{src}/a.ts` | ✅     |
| `{src}/*.ts` | `src/a.ts`   | ❌     |
| `{1..3}.txt` | `1.txt`      | ❌     |
| `{1..3}.txt` | `{1..3}.txt` | ✅     |
| `\{a,b\}`    | `{a,b}`      | ✅     |

To match a name that has a comma inside braces, escape the `{`: `\{a,b\}` (or just `\{a,b}`) is plain text. Instead of `{1..3}`, write `{1,2,3}` or the class `[1-3]`.

Spec: [GLOB_SPEC §10.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Unclosed braces are errors

A `{` always opens a brace, and the brace must be closed. `{a,b` and `src/{a,b/*.ts` fail with `UnterminatedBrace`. globstar does not guess what you meant. See [Compile-time errors](./errors.md).

A `}` with no open brace is fine. It is a plain character, so `{a,b}}` matches `a}` and `b}`.

Spec: [GLOB_SPEC §10.6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## `!` inside a brace

The leading `!` (negation) is read before braces are expanded. A `!` at the start of a branch is a plain character, so `{!a,b}` matches `!a` and `b`, but not `a`. See [Pattern syntax](./syntax.md#negation-with-a-leading).

## Braces and `**`

A branch may contain its own `**`. Each branch is separate, so the `**` in one branch does not affect another.

| Pattern                     | Path         | Match? |
| --------------------------- | ------------ | ------ |
| `{src/**/*.ts,lib/**/*.js}` | `src/a/b.ts` | ✅     |
| `{src/**/*.ts,lib/**/*.js}` | `lib/c.js`   | ✅     |
| `{src/**/*.ts,lib/**/*.js}` | `lib/c.ts`   | ❌     |

A `**` right next to a brace, or at the edge of a branch, is judged on each expansion: `{**,dist}/*.js` expands to `**/*.js`, where `**` owns a segment, while `src{**,x}.ts` expands to `src**.ts`, where it acts as `*`. See [`**` next to a brace](./globstar.md#next-to-a-brace).

Spec: [GLOB_SPEC §10.7, §9.6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Braces and character classes

A branch may hold a class, as in `{[0-9],x}.log`. Inside a class, `{`, `}`, and `,` are plain characters. See [Classes and braces](./character-classes.md#classes-and-braces) for examples.

Spec: [GLOB_SPEC §10.7](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Limits

Most brace patterns have no size limit. Three limits exist to stop a hostile or broken pattern from using too much time or memory: nesting depth, the expansion budget around `**`, and the compile-work bound on braces whose branches cross a `/` (`TooComplex`, described in [Errors and limits](./errors.md#the-four-limits)). When one is hit, compiling throws a `GlobError`, and its `max` field holds the limit.

### Nesting depth: 32

Braces may be nested up to 32 levels deep. One more level fails with `BraceNestingTooDeep`. Every `{` counts as it opens, even one with no comma that ends up literal. Real patterns rarely go past three or four levels.

A `**` next to braces can create nesting you did not write yourself, and that nesting counts too. For example, `**` followed by `{,/a}` written 33 times fails with the same error.

Spec: [GLOB_SPEC §6.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### Expansion budget: 4096

The number of branches has no limit: a brace with 5,000 branches compiles fine, and so does a product of braces within one segment.

A `**` next to a brace is different. To judge that `**` per expansion, globstar copies the parts of the pattern around it. It counts every copy, and when the count would pass 4096, compiling fails with `BraceExpansionTooLarge`. The count covers the whole pattern, and all patterns of one list together.

| Pattern                                              | Is it copied?                                         |
| ---------------------------------------------------- | ----------------------------------------------------- |
| `{**,a}{/,b}` written 5 times in a row               | yes: each link copies the chain so far, so this fails |
| `{**,dist}{/a,/b,…,.map}` with a long mixed list     | yes: the list is copied once, so a big list fails     |
| `{**,dist}{/a,/b,…}`, every branch starting with `/` | no                                                    |
| `**{/a,/b,…}`, every branch starting with `/`        | no                                                    |
| `**/{a,b,…}`, a `/` between `**` and the brace       | no                                                    |

::: tip Staying under the budget
If a long list must follow a `**`, put a `/` between them (`**/{a,b,…}`), or start every branch with `/` (`{**,dist}{/a,/b,…}`). Neither form copies the list.
:::

Spec: [GLOB_SPEC §6.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
