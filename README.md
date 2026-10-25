<h1 align="center">globstar</h1>

<div align="center">

[![MIT licensed][badge-license]][url-license]
[![npm version][badge-npm]][url-npm]
[![crates.io version][badge-crates]][url-crates]
[![CI][badge-ci]][url-ci]

</div>

<p align="center">A compiled, segment-based glob matcher, identical in Rust and JavaScript.</p>

[badge-license]: https://img.shields.io/badge/license-MIT-blue.svg
[url-license]: ./LICENSE
[badge-npm]: https://img.shields.io/npm/v/%40globstar%2Fcore?label=npm
[url-npm]: https://www.npmjs.com/package/@globstar/core
[badge-crates]: https://img.shields.io/crates/v/globstar?label=crates.io
[url-crates]: https://crates.io/crates/globstar
[badge-ci]: https://github.com/shulaoda/globstar/actions/workflows/ci.yml/badge.svg
[url-ci]: https://github.com/shulaoda/globstar/actions/workflows/ci.yml

Rust and Node each have several glob libraries, and no two agree on
every edge: a `**` beside text, a brace that crosses a `/`, a leading
dot, a class, a stray separator. A tool that matches in one language
and again in the other gets two sets of files. `globstar` is one
dialect, written down as a spec, with two implementations that match
byte for byte: the Rust crate is canonical, and a differential corpus
holds the JavaScript port to it.

```
crates/globstar          — Rust crate (canonical implementation)
crates/globstar-walk     — Rust filesystem walker built on globstar
packages/globstar        — JavaScript matcher published as @globstar/core
packages/globstar-walk   — JavaScript walker published as @globstar/walk
```

## Syntax

| Syntax           | Meaning                                                                                                                                                                                              |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `?`              | One byte that is not a separator.                                                                                                                                                                    |
| `*`              | Zero or more bytes within one segment; never crosses a `/`.                                                                                                                                          |
| `**`             | Any number of whole segments, zero included, when it stands alone between `/` and the pattern's ends (`a/**/b`, `**/x`). Beside other text it is a `*`. A leading `**/` also takes an absolute root. |
| `/`              | Exactly one separator. Only the `/` beside a `**` accepts a run of them.                                                                                                                             |
| `[abc]`, `[a-z]` | One byte from the set or the range; `[!abc]` or `[^abc]` any byte but those. A class never matches a separator.                                                                                      |
| `{a,b}`          | One of the branches. A branch may hold wildcards, nest braces or be empty (`{a,}`); `{a}` with no comma is the literal text `{a}`.                                                                   |
| `!`              | At the start of the pattern, negates the whole pattern; `!!` cancels. Anywhere else it is literal.                                                                                                   |
| `\`              | The next byte, literally (`\*`, `\{`); `\/` is an error.                                                                                                                                             |

## Options

| Option                                    | Meaning                                         | Matcher | Walker  |
| ----------------------------------------- | ----------------------------------------------- | ------- | ------- |
| `dot`                                     | A wildcard may take the `.` that starts a name. | `true`  | `false` |
| `case_insensitive` (JS `caseInsensitive`) | ASCII letters compare case-folded.              | `false` | `false` |

## Limits

Each pattern is at most 64 KiB and its braces nest at most 32 deep; a
union's members share no limit. Everything within those limits
compiles, in time and memory linear in its length.

## Documentation

Guide, playground and the spec: **<https://globstar.void.app>**

## Status

Every package stays on `0.0.x` while the spec settles. Any release may
change syntax, behavior or the public API with no migration path; a
behavior is stable only once the corpus has a row for it.
