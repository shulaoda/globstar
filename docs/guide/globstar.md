# The `**` wildcard

`**` is the only wildcard that crosses `/`. This page explains when `**` counts as a real "any depth" wildcard, what it matches at the start, middle, and end of a pattern, and how it behaves next to braces. For the rest of the syntax, see [Pattern syntax](./syntax.md).

All examples use the default options (`dot: true`, `caseInsensitive: false`) unless a table says otherwise.

## What `**` matches

A real `**` matches zero or more whole path segments. "Zero" matters: `src/**/*.ts` matches `src/index.ts`, where `**` matches nothing at all. On its own, `**` matches any path, even the empty one.

| Pattern            | Path                       | Match? |
| ------------------ | -------------------------- | ------ |
| `src/**/*.ts`      | `src/index.ts`             | ✅     |
| `src/**/*.ts`      | `src/a/b/c.ts`             | ✅     |
| `src/**/*.ts`      | `lib/a.ts`                 | ❌     |
| `src/**/test/*.ts` | `src/test/a.ts`            | ✅     |
| `src/**/test/*.ts` | `src/a/b/test/c.ts`        | ✅     |
| `src/**/test/*.ts` | `src/test/a/b.ts`          | ❌     |
| `**`               | `.github/workflows/ci.yml` | ✅     |

The last `src/**/test/*.ts` row fails because `*` after `test/` still matches only one segment.

Spec: [GLOB_SPEC §8.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## `**` must own a whole segment

`**` is a real "any depth" wildcard only when it fills a whole segment:

- before it comes a `/` or the start of the pattern, **and**
- after it comes a `/` or the end of the pattern.

Anywhere else, `**` is just two `*` in a row, and that means the same as one `*`. It stays inside one segment.

| Pattern       | Means the same as | Real `**`? |
| ------------- | ----------------- | ---------- |
| `src/**/*.ts` | —                 | yes        |
| `**/*.ts`     | —                 | yes        |
| `src/**`      | —                 | yes        |
| `**`          | —                 | yes        |
| `src/**.ts`   | `src/*.ts`        | no         |
| `**.js`       | `*.js`            | no         |
| `src**/x`     | `src*/x`          | no         |
| `a**b`        | `a*b`             | no         |

| Pattern     | Path         | Match? |
| ----------- | ------------ | ------ |
| `src/**.ts` | `src/a.ts`   | ✅     |
| `src/**.ts` | `src/a/b.ts` | ❌     |
| `**.js`     | `a.js`       | ✅     |
| `**.js`     | `lib/a.js`   | ❌     |
| `a**b`      | `axb`        | ✅     |
| `a**b`      | `a/b`        | ❌     |

::: warning `**.js` is not "every `.js` file"
A `**` glued to other characters loses its power. To match `.js` files at any depth, write `**/*.js`.
:::

Spec: [GLOB_SPEC §8.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## A trailing `/**` and the directory itself

`dist/**` means "`dist/` followed by anything". The `dist/` part is required, so the bare path `dist` does not match. The path `dist/` does match: `**` matches the empty rest.

| Pattern          | Path                   | Match? |
| ---------------- | ---------------------- | ------ |
| `dist/**`        | `dist`                 | ❌     |
| `dist/**`        | `dist/`                | ✅     |
| `dist/**`        | `dist/app.js`          | ✅     |
| `dist/**`        | `dist/assets/logo.svg` | ✅     |
| `{dist,dist/**}` | `dist`                 | ✅     |
| `{dist,dist/**}` | `dist/assets/logo.svg` | ✅     |

To match the directory and everything in it, list both, as in `{dist,dist/**}`. When a file walker asks "should I enter this directory?", it uses `matchDir` instead of `match`; see [Directory matching](./directory-matching.md).

Spec: [GLOB_SPEC §8.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## A leading `**/` means "at any depth"

A pattern that starts with `**/` matches the rest at any depth, including depth zero. It also accepts absolute paths, UNC paths, and Windows drive paths, so `**/*.rs` really means "every `.rs` file".

| Pattern              | Path                          | Match? |
| -------------------- | ----------------------------- | ------ |
| `**/node_modules`    | `node_modules`                | ✅     |
| `**/node_modules`    | `packages/web/node_modules`   | ✅     |
| `**/node_modules`    | `/home/me/app/node_modules`   | ✅     |
| `**/node_modules`    | `//server/share/node_modules` | ✅     |
| `**/node_modules`    | `C:/proj/node_modules`        | ✅     |
| `**/node_modules`    | `node_modules/react`          | ❌     |
| `**/node_modules/**` | `node_modules/react/index.js` | ✅     |

The `node_modules/react` row fails because the whole path must match. Add `/**` to also match what is inside.

To match **only** absolute paths, start the pattern with `/`:

| Pattern    | Path           | Match? |
| ---------- | -------------- | ------ |
| `/**/*.rs` | `src/main.rs`  | ❌     |
| `/**/*.rs` | `/src/main.rs` | ✅     |
| `**/*.rs`  | `/src/main.rs` | ✅     |

When a pattern starts with a brace, each branch decides this by itself. In `{**/package.json,README.md}`, the first branch accepts absolute paths and the second, a plain literal, does not:

| Pattern                       | Path                 | Match? |
| ----------------------------- | -------------------- | ------ |
| `{**/package.json,README.md}` | `/repo/package.json` | ✅     |
| `{**/package.json,README.md}` | `README.md`          | ✅     |
| `{**/package.json,README.md}` | `/README.md`         | ❌     |

Spec: [GLOB_SPEC §8.5](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Separators around `**`

Normally each `/` in a pattern matches exactly one separator. A `/` next to a real `**` is more relaxed: it also accepts a run of separators, so `src/**/index.ts` matches `src//index.ts`.

| Pattern           | Path            | Match? |
| ----------------- | --------------- | ------ |
| `src/**/index.ts` | `src//index.ts` | ✅     |
| `src/index.ts`    | `src//index.ts` | ❌     |

Spec: [GLOB_SPEC §12.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Repeated `**/**/`

Two or more `**/` in a row mean the same as one. `**/**/*.ts` is `**/*.ts`, and `a/**/**/b` is `a/**/b`.

| Pattern      | Path         | Match? |
| ------------ | ------------ | ------ |
| `**/**/*.ts` | `a.ts`       | ✅     |
| `**/**/*.ts` | `src/a/b.ts` | ✅     |
| `a/**/**/b`  | `a/b`        | ✅     |
| `a/**/**/b`  | `a/x/y/b`    | ✅     |

Spec: [GLOB_SPEC §8.6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## `**` and dot files

With `dot: false`, `**` does not step into names that start with `.`, and neither do the wildcards after it. See [How a path is matched](./matching.md).

| Pattern  | Path       | `dot: true` | `dot: false` |
| -------- | ---------- | ----------- | ------------ |
| `**`     | `.env`     | ✅          | ❌           |
| `src/**` | `src/.env` | ✅          | ❌           |
| `**/*`   | `a/.b`     | ✅          | ❌           |
| `**/*`   | `a/b`      | ✅          | ✅           |

Spec: [GLOB_SPEC §12.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## `**` and braces

### Each branch has its own `**`

A brace branch may contain `**`. Each branch is a pattern of its own, and a `**` in one branch does not affect another.

| Pattern                     | Path         | Match? |
| --------------------------- | ------------ | ------ |
| `{src/**/*.ts,lib/**/*.js}` | `src/a/b.ts` | ✅     |
| `{src/**/*.ts,lib/**/*.js}` | `lib/x/y.js` | ✅     |
| `{src/**/*.ts,lib/**/*.js}` | `src/a.js`   | ❌     |

Spec: [GLOB_SPEC §7.8, §8.7](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### `**` next to a brace

A brace means "any one of these branches" (see [Brace expansion](./braces.md)). To check whether a `**` next to a brace owns a segment, write out each choice as a separate pattern and apply the normal rule to each one. The same `**` can be a real `**` in one choice and a plain `*` in another.

| Pattern          | Write out each choice    | Result                 |
| ---------------- | ------------------------ | ---------------------- |
| `{**,dist}/*.js` | `**/*.js` or `dist/*.js` | real `**` in the first |
| `src{**,x}.ts`   | `src**.ts` or `srcx.ts`  | `**` acts as `*`       |
| `**{/a,b}`       | `**/a` or `**b`          | real `**`, then `*`    |
| `{a/,b}**`       | `a/**` or `b**`          | real `**`, then `*`    |
| `{a/**,x}c`      | `a/**c` or `xc`          | `**` acts as `*`       |

| Pattern          | Path        | Match? |
| ---------------- | ----------- | ------ |
| `{**,dist}/*.js` | `a/b/c.js`  | ✅     |
| `{**,dist}/*.js` | `dist/a.js` | ✅     |
| `src{**,x}.ts`   | `srcfoo.ts` | ✅     |
| `src{**,x}.ts`   | `src/a.ts`  | ❌     |
| `**{/a,b}`       | `x/y/a`     | ✅     |
| `**{/a,b}`       | `xb`        | ✅     |
| `**{/a,b}`       | `x/b`       | ❌     |
| `{a/,b}**`       | `a/x/y`     | ✅     |
| `{a/,b}**`       | `b/x`       | ❌     |
| `{a/**,x}c`      | `a/z/c`     | ❌     |

Writing out the choices can copy parts of the pattern, so globstar limits how much copying a `**` next to a brace may cause. See [Brace expansion](./braces.md) for the limit.

Spec: [GLOB_SPEC §7.0, §8.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### `**` inside a brace with no comma

A brace with no comma is not a brace: `{**}` is the plain characters `{`, `**`, `}`. Here `**` sits between `{` and `}`, so it acts as `*`. A `**` with `/` on both sides is still real, even inside such literal braces.

| Pattern    | Path        | Match? |
| ---------- | ----------- | ------ |
| `{**}`     | `{a}`       | ✅     |
| `{**}`     | `{a/b}`     | ❌     |
| `{a/**/b}` | `{a/x/y/b}` | ✅     |

Spec: [GLOB_SPEC §7.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
