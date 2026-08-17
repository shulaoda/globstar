# @globstar/walk

Filesystem walker powered by [`@globstar/core`]. Static-prefix
jump-start, `match_dir` subtree pruning, symlink cycle breaking,
`!`-pattern auto-split into the ignore set.

```js
import { glob } from "@globstar/walk";

const files = await glob(["src/**", "!src/generated/**"]);
```

The Rust twin is the [`globstar-walk`] crate. Specs and the shared
corpus live in the [repository](https://github.com/shulaoda/globstar).

[`@globstar/core`]: https://www.npmjs.com/package/@globstar/core
[`globstar-walk`]: https://crates.io/crates/globstar-walk
