# @globstar/core

Cross-platform, high-performance glob matching. Pure string matching —
no filesystem access, no `node:` imports; runs in any JS runtime.

```js
import { globstar } from "@globstar/core";

const isSource = globstar("src/**");
isSource("src/foo.ts"); // true
```

`matchDir` answers three questions about a directory: does it match,
may something below it match, and does everything below it match. A
pattern that excludes files may skip a directory only on the third:

```js
import { compileMatcher, DirMatch } from "@globstar/core";

const exclude = compileMatcher(["**/node_modules/**", "**/*.log"]);
// Everything below is excluded: skip the directory whole.
DirMatch.matchesAllBelow(exclude.matchDir("/p/node_modules")); // true
// The directory matches, the files in it don't: keep it.
DirMatch.matchesAllBelow(exclude.matchDir("/p/foo.log")); // false
```

The behaviorally identical Rust twin is the [`globstar`] crate; the
filesystem walker built on this package is [`@globstar/walk`]. Dialect
spec and the shared golden corpus live in the
[repository](https://github.com/shulaoda/globstar).

[`globstar`]: https://crates.io/crates/globstar
[`@globstar/walk`]: https://www.npmjs.com/package/@globstar/walk
