# Directory matching

A filesystem walker meets directories before it meets the files inside them. `matchDir` (Rust: `match_dir`) lets it ask a pattern about a directory path and skip whole subtrees safely. This page explains the three facts a verdict carries, the six verdicts, and how a pattern that _selects_ files reads them differently from a pattern that _excludes_ files.

## Three facts, six verdicts

For a directory path `d`, a pattern answers three questions:

| Fact                | Question                                                                        | Which way it errs                               |
| ------------------- | ------------------------------------------------------------------------------- | ----------------------------------------------- |
| `matches`           | Does `d` itself match, exactly as a file path would?                            | Exact.                                          |
| `may match below`   | Could some path `d/…` match?                                                    | `false` is a guarantee; `true` may be cautious. |
| `matches all below` | Does _every_ path `d/…` match, for every `…` of one or more non-empty segments? | `true` is a guarantee; `false` may be cautious. |

"All below" implies "may below", so the three bits form six consistent combinations. Each is a verdict:

| Verdict              | matches | may match below | matches all below | Example                               |
| -------------------- | :-----: | :-------------: | :---------------: | ------------------------------------- |
| `Pruned`             |   no    |       no        |        no         | `src/**/*.ts` against `lib`           |
| `Match`              |   yes   |       no        |        no         | `src` against `src`                   |
| `Descend`            |   no    |       yes       |        no         | `src/**/*.ts` against `src`           |
| `DescendAndMatch`    |   yes   |       yes       |        no         | `src{,/**/*.ts}` against `src`        |
| `DescendAll`         |   no    |       yes       |        yes        | `dist/**` against `dist` (with `dot`) |
| `DescendAllAndMatch` |   yes   |       yes       |        yes        | `**` against `a`                      |

The names read from the pattern's side: the pattern _matches_ the directory, _descends_ below it, or is _pruned_ there. What a walker does with a verdict depends on the role the pattern plays, so walkers read the facts, not the names:

::: code-group

```js [JavaScript]
import { compileMatcher, DirMatch } from "@globstar/core";

const d = compileMatcher("src/**/*.ts").matchDir("src");
DirMatch.matches(d); // false
DirMatch.mayMatchBelow(d); // true
DirMatch.matchesAllBelow(d); // false
```

```rust [Rust]
let d = Glob::new("src/**/*.ts")?.match_dir(b"src");
assert!(!d.matches());
assert!(d.may_match_below());
assert!(!d.matches_all_below());
```

:::

Spec: [GLOB_SPEC §13.1–13.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## A pattern that selects files

For an _include_ pattern, the walker wants to know whether a directory is worth entering. It reads `may match below`: when that is `false`, nothing under the directory can match, and the subtree is skipped. When `matches all below` is `true`, every file below is a result, and the walker may stop testing files one by one.

| Pattern               | Directory      | Verdict      | Walker action                       |
| --------------------- | -------------- | ------------ | ----------------------------------- |
| `src/**/*.ts`         | `src`          | `Descend`    | Enter; test files inside.           |
| `src/**/*.ts`         | `src/engine`   | `Descend`    | Enter.                              |
| `src/**/*.ts`         | `node_modules` | `Pruned`     | Skip the whole subtree.             |
| `src/*.ts`            | `src`          | `Descend`    | Enter.                              |
| `src/*.ts`            | `src/a`        | `Pruned`     | Skip: `*` cannot cross the `/`.     |
| `src/**` (with `dot`) | `src`          | `DescendAll` | Enter; every file below is a match. |

```js
const include = compileMatcher(["src/**/*.ts", "docs/**/*.md"]);
for (const dir of ["", "src", "docs/guide", "node_modules"]) {
  DirMatch.mayMatchBelow(include.matchDir(dir)); // true, true, true, false
}
```

The empty string is the walk root: it always may match below.

## A pattern that excludes files

For an _exclude_ pattern, the questions turn around. The walker wants to know whether a directory can be dropped whole; that is `matches all below`. It also wants to know when it can stop testing this pattern under a directory; that is `may match below` being `false`.

| Pattern                           | Directory          | Verdict           | Walker action                                           |
| --------------------------------- | ------------------ | ----------------- | ------------------------------------------------------- |
| `**/node_modules/**` (with `dot`) | `node_modules`     | `DescendAll`      | Everything below is excluded: skip the directory.       |
| `**/node_modules/**` (with `dot`) | `a/b/node_modules` | `DescendAll`      | Skip.                                                   |
| `**/node_modules/**` (with `dot`) | `src`              | `Descend`         | Enter; some files below may still be excluded.          |
| `**/*.log`                        | `app.log`          | `DescendAndMatch` | Enter! The directory matches, the files below need not. |
| `dist/**`                         | `src`              | `Pruned`          | Enter, and stop testing this exclude below `src`.       |

The `**/*.log` row is the trap this API is designed around. A pattern meant for files can happen to match a directory's own path. That says nothing about the files inside it: `app.log/notes.txt` does not end in `.log`. An exclude list must only skip a directory on `matches all below`, never on `matches`.

```js
const exclude = compileMatcher(["**/node_modules/**", "**/*.log"]);
DirMatch.matchesAllBelow(exclude.matchDir("node_modules")); // true  → skip whole
DirMatch.matchesAllBelow(exclude.matchDir("app.log")); // false → keep walking
```

::: tip The same six verdicts serve both roles
A verdict is a statement about the pattern, not a command to the walker. `Pruned` from an include means "skip this directory"; `Pruned` from an exclude means "this exclude needs no more tests here". The facts carry the meaning; the names are only labels.
:::

## The `dot` option never covers a subtree

With `dot: false`, no wildcard may match a name that starts with `.`. A directory can then always hold a dotfile the pattern does not match, so `matches all below` is never `true`. The walkers default to `dot: false`; an exclude list that should drop `node_modules` whole must compile with `dot: true`.

| Pattern              | Directory      | Options      | Verdict      |
| -------------------- | -------------- | ------------ | ------------ |
| `**/node_modules/**` | `node_modules` | `dot: true`  | `DescendAll` |
| `**/node_modules/**` | `node_modules` | `dot: false` | `Descend`    |
| `dist/**`            | `dist`         | `dot: true`  | `DescendAll` |
| `dist/**`            | `dist`         | `dot: false` | `Descend`    |

Spec: [GLOB_SPEC §13.6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Negated patterns

A pattern with a leading `!` matches every path its body does not match. What it excludes below a directory cannot be told from the directory itself, so its verdict is always `Descend`.

```js
compileMatcher("!**/*.ts").matchDir("src"); // DirMatch.Descend
compileMatcher("!**/*.ts").matchDir("node_modules"); // DirMatch.Descend
```

Spec: [GLOB_SPEC §13.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Union of patterns

For a matcher built from several patterns, the verdict is computed on the union, not per member. If any member may match below, the union may. "All below" is a property of the whole set, so members that each cover a part can cover together. See [Multi-pattern matchers](./multi-pattern.md).

| Patterns        | Directory | Verdict      |
| --------------- | --------- | ------------ |
| `a/*`           | `a`       | `Descend`    |
| `a/*/**`        | `a`       | `Descend`    |
| `a/*`, `a/*/**` | `a`       | `DescendAll` |

## How a path is queried

Walkers get directory names from `readdir` without a trailing separator, and that is the canonical form to ask about: `matchDir("src")`, not `matchDir("src/")`. The directory path is not normalized; a trailing `/` asks about that exact byte string. The empty string is the root of the walk.

## Static prefixes

`staticPrefixes()` returns the literal path prefixes every match starts with, so a walk can begin there instead of at the root. `src/**/*.ts` and `docs/**/*.md` together give `src` and `docs`; `**/*.ts` gives the empty prefix, meaning the root.

```js
compileMatcher(["src/**/*.ts", "docs/**/*.md"]).staticPrefixes(); // [src, docs] as Uint8Array
```

Spec: [GLOB_SPEC §13.5](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
