# The `**` wildcard

`**` is the only wildcard that crosses `/`. This page explains when `**` counts as a real "any depth" wildcard, what it matches at the start, middle, and end of a pattern, and how it behaves next to braces. For the rest of the syntax, see [Pattern syntax](./syntax.md).

All examples use the default options (`dot: true`, `caseInsensitive: false`) unless a table says otherwise.

## What `**` matches

A real `**` matches zero or more whole path segments. Zero segments count: `src/**/*.ts` matches `src/index.ts`, where `**` matches nothing. On its own, `**` matches any path, even the empty one.

| Pattern            | Path                       | Match? |
| ------------------ | -------------------------- | ------ |
| `src/**/*.ts`      | `src/index.ts`             | yes    |
| `src/**/*.ts`      | `src/a/b/c.ts`             | yes    |
| `src/**/*.ts`      | `lib/a.ts`                 | no     |
| `src/**/test/*.ts` | `src/test/a.ts`            | yes    |
| `src/**/test/*.ts` | `src/a/b/test/c.ts`        | yes    |
| `src/**/test/*.ts` | `src/test/a/b.ts`          | no     |
| `**`               | `.github/workflows/ci.yml` | yes    |

The last `src/**/test/*.ts` row fails because `*` after `test/` still matches only one segment.

Spec: [GLOB_SPEC §9.2](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

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
| `src/**.ts` | `src/a.ts`   | yes    |
| `src/**.ts` | `src/a/b.ts` | no     |
| `**.js`     | `a.js`       | yes    |
| `**.js`     | `lib/a.js`   | no     |
| `a**b`      | `axb`        | yes    |
| `a**b`      | `a/b`        | no     |

::: warning `**.js` is not every `.js` file
A `**` joined to other characters is a plain `*`. To match `.js` files at any depth, write `**/*.js`.
:::

Spec: [GLOB_SPEC §9.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## A trailing `/**` and the directory itself

`dist/**` means "`dist/` followed by anything". The `dist/` part is required, so the bare path `dist` does not match. The path `dist/` does match: `**` matches the empty rest.

| Pattern          | Path                   | Match? |
| ---------------- | ---------------------- | ------ |
| `dist/**`        | `dist`                 | no     |
| `dist/**`        | `dist/`                | yes    |
| `dist/**`        | `dist/app.js`          | yes    |
| `dist/**`        | `dist/assets/logo.svg` | yes    |
| `{dist,dist/**}` | `dist`                 | yes    |
| `{dist,dist/**}` | `dist/assets/logo.svg` | yes    |

To match the directory and everything in it, list both, as in `{dist,dist/**}`. When a file walker asks "should I enter this directory?", it uses `matchDir` instead of `match`; see [Directory matching](./directory-matching.md).

Spec: [GLOB_SPEC §9.3](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## A leading `**/` means "at any depth"

A pattern that starts with `**/` matches the rest at any depth, including depth zero. It also accepts absolute paths, UNC paths, and Windows drive paths, so `**/*.rs` really means "every `.rs` file".

| Pattern              | Path                          | Match? |
| -------------------- | ----------------------------- | ------ |
| `**/node_modules`    | `node_modules`                | yes    |
| `**/node_modules`    | `packages/web/node_modules`   | yes    |
| `**/node_modules`    | `/home/me/app/node_modules`   | yes    |
| `**/node_modules`    | `//server/share/node_modules` | yes    |
| `**/node_modules`    | `C:/proj/node_modules`        | yes    |
| `**/node_modules`    | `node_modules/react`          | no     |
| `**/node_modules/**` | `node_modules/react/index.js` | yes    |

The `node_modules/react` row fails because the whole path must match. Add `/**` to also match what is inside.

To match **only** absolute paths, start the pattern with `/`:

| Pattern    | Path           | Match? |
| ---------- | -------------- | ------ |
| `/**/*.rs` | `src/main.rs`  | no     |
| `/**/*.rs` | `/src/main.rs` | yes    |
| `**/*.rs`  | `/src/main.rs` | yes    |

When a pattern starts with a brace, each branch decides this by itself. In `{**/package.json,README.md}`, the first branch accepts absolute paths and the second, a plain literal, does not:

| Pattern                       | Path                 | Match? |
| ----------------------------- | -------------------- | ------ |
| `{**/package.json,README.md}` | `/repo/package.json` | yes    |
| `{**/package.json,README.md}` | `README.md`          | yes    |
| `{**/package.json,README.md}` | `/README.md`         | no     |

Spec: [GLOB_SPEC §9.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Separators around `**`

Normally each `/` in a pattern matches exactly one separator. A `/` next to a real `**` is more relaxed: it also accepts a run of separators, so `src/**/index.ts` matches `src//index.ts`.

| Pattern           | Path            | Match? |
| ----------------- | --------------- | ------ |
| `src/**/index.ts` | `src//index.ts` | yes    |
| `src/index.ts`    | `src//index.ts` | no     |

Spec: [GLOB_SPEC §9.5](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Repeated `**/**/`

Two or more `**/` in a row mean the same as one. `**/**/*.ts` is `**/*.ts`, and `a/**/**/b` is `a/**/b`.

| Pattern      | Path         | Match? |
| ------------ | ------------ | ------ |
| `**/**/*.ts` | `a.ts`       | yes    |
| `**/**/*.ts` | `src/a/b.ts` | yes    |
| `a/**/**/b`  | `a/b`        | yes    |
| `a/**/**/b`  | `a/x/y/b`    | yes    |

Spec: [GLOB_SPEC §9.6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## `**` and dot files

With `dot: false`, `**` does not step into names that start with `.`, and neither do the wildcards after it. See [How a path is matched](./matching.md).

| Pattern  | Path       | `dot: true` | `dot: false` |
| -------- | ---------- | ----------- | ------------ |
| `**`     | `.env`     | yes         | no           |
| `src/**` | `src/.env` | yes         | no           |
| `**/*`   | `a/.b`     | yes         | no           |
| `**/*`   | `a/b`      | yes         | yes          |

Spec: [GLOB_SPEC §12](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## `**` and braces

A brace branch may contain its own `**`; each branch is a pattern of its own (see [Braces and `**`](./braces.md#braces-and)). What needs care is a `**` at the edge of a brace.

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
| `{**,dist}/*.js` | `a/b/c.js`  | yes    |
| `{**,dist}/*.js` | `dist/a.js` | yes    |
| `src{**,x}.ts`   | `srcfoo.ts` | yes    |
| `src{**,x}.ts`   | `src/a.ts`  | no     |
| `**{/a,b}`       | `x/y/a`     | yes    |
| `**{/a,b}`       | `xb`        | yes    |
| `**{/a,b}`       | `x/b`       | no     |
| `{a/,b}**`       | `a/x/y`     | yes    |
| `{a/,b}**`       | `b/x`       | no     |
| `{a/**,x}c`      | `a/z/c`     | no     |

Spec: [GLOB_SPEC §10.1, §9.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

### `**` inside a brace with no comma

A brace with no comma is not a brace: `{**}` is the plain characters `{`, `**`, `}`. Here `**` sits between `{` and `}`, so it acts as `*`. A `**` with `/` on both sides is still real, even inside such literal braces.

| Pattern    | Path        | Match? |
| ---------- | ----------- | ------ |
| `{**}`     | `{a}`       | yes    |
| `{**}`     | `{a/b}`     | no     |
| `{a/**/b}` | `{a/x/y/b}` | yes    |

Spec: [GLOB_SPEC §10.4](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
