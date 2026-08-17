# globstar

Cross-platform, high-performance glob matching. Compile a pattern once,
match paths as raw bytes — `*`, `?`, `**`, classes, braces, escapes,
leading-`!` negation, plus first-class `match_dir` pruning and
`static_prefixes` seeding for walkers.

```rust
use globstar::Glob;

let glob = Glob::new("src/**/*.rs")?;
assert!(glob.is_match(b"src/engine/mod.rs"));
```

The behaviorally identical JS twin is [`@globstar/core`]; the
filesystem walker built on this crate is [`globstar-walk`]. Dialect
spec, theory notes, and the shared golden corpus live in the
[repository](https://github.com/shulaoda/globstar).

[`@globstar/core`]: https://www.npmjs.com/package/@globstar/core
[`globstar-walk`]: https://crates.io/crates/globstar-walk
