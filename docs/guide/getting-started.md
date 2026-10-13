# Getting started

`globstar` is one glob dialect with two implementations: the Rust crate `globstar` and the JavaScript package `@globstar/core`. Both compile a pattern once and then answer the same three questions about any path: does this path match, what does this directory mean for the pattern, and where can a filesystem walk start. This page installs the library and shows each question once; the rest of the guide explains the rules behind the answers.

::: warning Project status
`globstar` is being built for [Vite](https://vitejs.dev/)'s `import.meta.glob`. Every release stays on `0.0.x`, and any `0.0.x` bump may change syntax, behavior or the API without a migration path. The [spec](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md) and the shared test corpus are the source of truth; a behavior is stable only when the corpus has a row for it.
:::

## Install

::: code-group

```sh [JavaScript]
pnpm add @globstar/core
# npm install @globstar/core
```

```sh [Rust]
cargo add globstar
```

:::

The JavaScript package is pure string matching: no filesystem access, no `node:` imports, so it runs in Node, Deno, Bun and the browser. The filesystem walkers live in separate packages, `@globstar/walk` and the `globstar-walk` crate.

## Match a path

A pattern is compiled once and reused. In JavaScript, `globstar()` returns a plain predicate; `compileMatcher()` returns an object with the directory API as well. In Rust, `Glob::new` returns a `Glob` that is `Send + Sync + Clone`.

::: code-group

```js [JavaScript]
import { globstar, compileMatcher } from "@globstar/core";

const isSource = globstar("src/**/*.ts");
isSource("src/engine/mod.ts"); // true
isSource("README.md"); // false

// The same predicate, plus matchDir() and staticPrefixes():
const m = compileMatcher("src/**/*.ts");
m.match("src/engine/mod.ts"); // true
```

```rust [Rust]
use globstar::Glob;

let glob = Glob::new("src/**/*.rs")?;
assert!(glob.is_match(b"src/engine/mod.rs"));
assert!(!glob.is_match(b"README.md"));
```

:::

Paths are matched whole, never as substrings, and `/` is the separator in patterns. On Windows a `\` in a path also counts as a separator. The details are in [How a path is matched](./matching.md).

## Options

Two options exist, and only two. `dot` decides whether wildcards may match a leading `.`; `caseInsensitive` folds ASCII letters. The matcher defaults to `dot: true`. The filesystem walkers default to `dot: false`, which is what shells do.

::: code-group

```js [JavaScript]
import { globstar } from "@globstar/core";

globstar("*")(".env"); // true  (dot defaults to true here)
globstar("*", { dot: false })(".env"); // false
globstar("*.TS", { caseInsensitive: true })("a.ts"); // true
```

```rust [Rust]
use globstar::{CompileOptions, Glob};

let opts = CompileOptions::default().dot(false).case_insensitive(true);
let glob = Glob::new_with("*.TS", opts)?;
assert!(glob.is_match(b"a.ts"));
assert!(!glob.is_match(b".env.ts"));
```

:::

## Several patterns at once

An array of patterns compiles into one matcher that answers with OR. In Rust this is `Glob::union`. See [Multi-pattern matchers](./multi-pattern.md) for how negated members and directory verdicts combine.

::: code-group

```js [JavaScript]
const m = compileMatcher(["src/**/*.ts", "docs/**/*.md"]);
m.match("docs/guide/intro.md"); // true
```

```rust [Rust]
let glob = Glob::union(["src/**/*.ts", "docs/**/*.md"])?;
assert!(glob.is_match(b"docs/guide/intro.md"));
```

:::

## Ask about a directory

A walker does not want to test every file under `node_modules` against `src/**/*.ts`. `matchDir` answers three questions about a directory path: does it match, may something below it match, and does everything below it match. The verdict is one of six values; a walker reads the facts it needs.

::: code-group

```js [JavaScript]
import { compileMatcher, DirMatch } from "@globstar/core";

const m = compileMatcher("src/**/*.ts");
m.matchDir("src"); // DirMatch.Descend: something below may match
m.matchDir("node_modules"); // DirMatch.Pruned: nothing below can

DirMatch.mayMatchBelow(m.matchDir("node_modules")); // false → skip it
```

```rust [Rust]
use globstar::{DirMatch, Glob};

let glob = Glob::new("src/**/*.ts")?;
assert_eq!(glob.match_dir(b"src"), DirMatch::Descend);
assert_eq!(glob.match_dir(b"node_modules"), DirMatch::Pruned);
assert!(!glob.match_dir(b"node_modules").may_match_below());
```

:::

[Directory matching](./directory-matching.md) explains the six verdicts and how a pattern that excludes files reads them differently from one that selects files. `staticPrefixes()` is the other walker helper: the literal prefixes every match starts with, so a walk can begin at `src` instead of the root.

## Where next

- [Pattern syntax](./syntax.md): every construct in one place, and what is deliberately not supported.
- [The `**` wildcard](./globstar.md): the rule that `**` must own a whole segment, and what `a/**` means for `a`.
- [Errors and limits](./errors.md): what fails to compile and why.
- [Playground](../playground.md): try a pattern against a list of paths in the browser.
