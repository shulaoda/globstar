# JavaScript API

`@globstar/core` exports two factory functions, the `DirMatch` helpers, and the `GlobError` class. It has no runtime dependencies, no filesystem access and no `node:` imports, so it runs in Node 20+ (it needs `String.prototype.isWellFormed`; the package asks for Node 22), Deno, Bun and browsers. Types ship with the package.

```js
import { globstar, compileMatcher, DirMatch, GlobError } from "@globstar/core";
```

## `globstar(patterns, options?)`

Compiles one pattern or an array of patterns into a predicate `(path: string) => boolean`. An array combines with OR; a negated member contributes its complement (see [Multi-pattern matchers](../guide/multi-pattern.md)). Throws `GlobError` when a pattern does not compile and `TypeError` when a pattern is not a string.

```js
const isSource = globstar("src/**/*.ts");
isSource("src/a.ts"); // true

paths.filter(globstar(["src/**", "lib/**"]));
```

## `compileMatcher(patterns, options?)`

Same inputs, returns a `Matcher` with the walker-facing surface:

| Member             | Returns         | Meaning                                                                                                                                                            |
| ------------------ | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `match(path)`      | `boolean`       | Whole-path match. The same predicate `globstar()` returns.                                                                                                         |
| `matchDir(path)`   | `DirMatchValue` | What the directory means for the pattern set. See [Directory matching](../guide/directory-matching.md).                                                            |
| `staticPrefixes()` | `Uint8Array[]`  | Literal prefixes every match starts with: each expansion's literal head up to its last whole segment, in the pattern's case. An empty `Uint8Array` means the root. |

`match` and `matchDir` throw `TypeError` on non-string input. The prefixes are bytes because paths are matched as bytes; decode with `new TextDecoder().decode(prefix)` when you need a string. Past a size bound, a head is a shorter prefix that still holds.

## Options

```ts
interface GlobstarOptions {
  dot?: boolean; // default true
  caseInsensitive?: boolean; // default false
}
```

- `dot`: when `false`, wildcards (`*`, `?`, negated classes, `**`) cannot match a leading `.` in a segment. The matcher defaults to `true`; the walker `@globstar/walk` defaults to `false`.
- `caseInsensitive`: ASCII-only case folding. Non-ASCII bytes compare exactly.

These are the only two options. The dialect has no toggles for `**` behavior, brace expansion or extglobs; see [How a path is matched](../guide/matching.md). A key set to `undefined` keeps its default. An unknown key (`nocase`), or a value that is not a boolean (`"false"`), throws a `TypeError` instead of being read loosely, and only the object's own keys count.

## `DirMatch`

An object holding the six verdicts as numbers and the helpers that read them.

```ts
DirMatch.Pruned; // 1   nothing at or below the directory matches
DirMatch.Match; // 0    the directory matches; nothing below can
DirMatch.Descend; // 2  something below may match
DirMatch.DescendAndMatch; // 3
DirMatch.DescendAll; // 4   everything below matches
DirMatch.DescendAllAndMatch; // 5
```

| Helper                             | Returns   | Meaning                                                                            |
| ---------------------------------- | --------- | ---------------------------------------------------------------------------------- |
| `DirMatch.matches(d)`              | `boolean` | The directory's own path matches.                                                  |
| `DirMatch.mayMatchBelow(d)`        | `boolean` | Some path below may match. `false` is a guarantee that none does.                  |
| `DirMatch.matchesAllBelow(d)`      | `boolean` | Every path below matches. `true` is a guarantee; always `false` with `dot: false`. |
| `DirMatch.from(matches, may, all)` | verdict   | Builds a verdict from the three facts; `all` implies `may`.                        |

The numeric constants are also exported by name: `MATCH`, `PRUNED`, `DESCEND`, `DESCEND_AND_MATCH`, `DESCEND_ALL`, `DESCEND_ALL_AND_MATCH`.

```js
const m = compileMatcher(["**/node_modules/**", "**/*.log"]);
const d = m.matchDir("node_modules");
if (DirMatch.matchesAllBelow(d)) {
  // every file below is excluded: skip the directory
}
```

## `GlobError`

Thrown by both factories when a pattern fails to compile. `kind` names the failure; the other fields depend on the kind.

```ts
class GlobError extends Error {
  readonly name: "GlobError";
  readonly kind: string; // "UnterminatedClass", "InvalidRange", ...
  readonly at?: number; // byte offset of the offending construct
  readonly len?: number; // pattern length, for TooLong
  readonly max?: number; // the limit that was hit
  readonly low?: number; // range bounds, for InvalidRange
  readonly high?: number;
}
```

```js
try {
  globstar("src/[a");
} catch (e) {
  if (e instanceof GlobError) console.error(e.kind, e.message);
  // UnterminatedClass unterminated character class at byte 4: ...
}
```

The full list of kinds, with an example for each, is in [Errors and limits](../guide/errors.md). An error from one member of a pattern list also has `index`, the member's position. `JSON.stringify(err)` keeps the message. The limits `MAX_PATTERN_LEN` and `MAX_BRACE_NESTING` are exported, and a pattern list may be any iterable of strings (an array, a `Set`, …).

## Walker

The filesystem walker is a separate package, `@globstar/walk`, built on `compileMatcher`. It seeds the walk from `staticPrefixes()`, prunes with `matchDir`, breaks symlink cycles, and splits `!`-patterns into its ignore set.

- Options: `cwd`, `dot`, `caseInsensitive`, `followSymlinks`, `ignore`, checked like the matcher's. `globSync` throws; `glob` rejects its Promise. An `ignore` entry may not start with `!`.
- A folder that links reach by several routes is walked once per route, as in every walker that follows links. Walk a tree that others control, such as a cloned repository or an unpacked archive, with `followSymlinks: false`.
- A pattern's literal head is opened as written. On a case-insensitive file system (macOS and Windows by default) it reaches names spelled in another case, past an `ignore` entry for the listed spelling (`.GIT/config` past `**/.git/**`). When the ignore list must hold against patterns from others, walk such a file system with `caseInsensitive: true`.

```js
import { glob } from "@globstar/walk";

const files = await glob(["src/**", "!src/generated/**"]);
```
