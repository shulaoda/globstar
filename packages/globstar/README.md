# @globstar/core

Cross-platform, high-performance glob matching. Pure string matching —
no filesystem access, no `node:` imports; runs in any JS runtime.

```js
import { globstar } from "@globstar/core";

const isSource = globstar("src/**");
isSource("src/foo.ts"); // true
```

The behaviorally identical Rust twin is the [`globstar`] crate; the
filesystem walker built on this package is [`@globstar/walk`]. Dialect
spec and the shared golden corpus live in the
[repository](https://github.com/shulaoda/globstar).

[`globstar`]: https://crates.io/crates/globstar
[`@globstar/walk`]: https://www.npmjs.com/package/@globstar/walk
