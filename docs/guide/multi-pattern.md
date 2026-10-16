# Multi-pattern matchers

Most callers have a list of patterns, not one. An array compiles into a single matcher that answers with boolean OR: a path matches when any member matches. This page covers what the union means, how negated members behave in each language, how directory verdicts combine, and the one budget the members share.

## One matcher, OR semantics

::: code-group

```js [JavaScript]
import { globstar, compileMatcher } from "@globstar/core";

const isDoc = globstar(["**/*.md", "docs/**"]);
isDoc("README.md"); // true
isDoc("docs/assets/logo.svg"); // true
isDoc("src/index.ts"); // false

const m = compileMatcher(["src/**/*.ts", "docs/**/*.md"]);
```

```rust [Rust]
use globstar::Glob;

let glob = Glob::union(["**/*.md", "docs/**"])?;
assert!(glob.is_match(b"README.md"));
assert!(glob.is_match(b"docs/assets/logo.svg"));
assert!(!glob.is_match(b"src/index.ts"));
```

:::

The union is OR, never AND. There is no API that tells you _which_ member matched; the matcher answers the boolean questions only (`match`, `matchDir`, `staticPrefixes`). A pattern given twice counts once. In Rust the union is an ordinary `Glob`, not a separate set type.

Spec: [GLOB_SPEC §14.1–14.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Negated members

A leading `!` makes a pattern match everything its body does not. Inside a union that is rarely what you want: the member contributes its _complement_ to the OR, it does not subtract from the other members.

```js
const m = globstar(["src/**", "!src/**/*.test.ts"]);
m("src/a.ts"); // true: matches src/**
m("src/a.test.ts"); // true: matches src/**
m("lib/x.js"); // true: matches the complement of src/**/*.test.ts
```

The two languages treat this differently:

- **JavaScript** accepts negated members and applies the rule above. `["src/**", "!src/**"]` matches every path.
- **Rust** rejects a negated member in `Glob::union` with `GlobError::NegatedInUnion`, because the result is almost never intended.

Include/exclude filtering belongs to the walker layer. `@globstar/walk` and `globstar-walk` split `!`-patterns out of the list into an ignore set, and that is the way to express "everything in `src`, except tests". See [Pattern syntax](./syntax.md) for how the leading `!` is counted.

::: warning
`["src/**", "!**/*.test.ts"]` as a matcher matches almost everything. Pass the list to a walker, or compile the include and exclude sets as two matchers and combine them yourself.
:::

## Directory verdicts on a union

`matchDir` is computed on the union, not per member and then merged by hand. The rule is conservative in the direction a walker needs:

- The union _may match below_ a directory if any member may.
- The union _matches_ a directory if any member matches it.
- The union _matches all below_ when the members together cover every path below, even when no single member does.

| Patterns             | Directory | Verdict      | Why                                             |
| -------------------- | --------- | ------------ | ----------------------------------------------- |
| `src/**`, `tests/**` | `lib`     | `Pruned`     | No member can match under `lib`.                |
| `src/**`, `tests/**` | `src`     | `DescendAll` | `src/**` alone covers everything below (`dot`). |
| `a/*`                | `a`       | `Descend`    | Only the children of `a` match.                 |
| `a/*/**`             | `a`       | `Descend`    | Only paths two or more levels down match.       |
| `a/*`, `a/*/**`      | `a`       | `DescendAll` | Together they cover every path below `a`.       |

Pruning is safe only when every member says `Pruned`. See [Directory matching](./directory-matching.md) for what each verdict means to an include list and to an exclude list.

Spec: [GLOB_SPEC §14.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Static prefixes of a union

`staticPrefixes()` returns each member's own literal heads (each expansion's literal head up to its last whole segment), shortest first, with prefixes covered by a shorter one removed. What the union shares does not shorten them: `["node_modules/react/package.json", "node_modules/vue/package.json"]` gives both full paths. A member that can match at the root (such as `**/*.ts`) makes the whole list the single empty prefix.

```js
compileMatcher(["src/**/*.ts", "docs/**/*.md"]).staticPrefixes(); // src, docs
compileMatcher(["src/**/*.ts", "**/*.md"]).staticPrefixes(); // "" (the root)
```

## The shared expansion budget

Braces next to `**` are taken apart at compile time, and the copies that makes are charged to one budget of 4096 units per compile: a single pattern, or a whole list together. So a long list of brace-heavy patterns can fail with `BraceExpansionTooLarge` where each member alone would compile. The compile-work limit (`TooComplex`) also covers the whole list, and it grows with the list's size; only braces whose branches cross a `/` and multiply ever reach it. Details are in [Brace expansion](./braces.md) and [Errors and limits](./errors.md).

## Errors

- An empty list fails with `EmptyPatternSet` in both languages.
- Any member that fails to compile fails the whole union with that member's error.
- Rust: a `!`-member fails with `NegatedInUnion`, which carries the member's index and text.

Spec: [GLOB_SPEC §10, §14](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
