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

`match_dir` answers three questions about a directory: does it match,
may something below it match, and does everything below it match. A
pattern that excludes files may skip a directory only on the third:

```rust
use globstar::Glob;

let exclude = Glob::union(["**/node_modules/**", "**/*.log"])?;
// Everything below is excluded: skip the directory whole.
assert!(exclude.match_dir(b"/p/node_modules").matches_all_below());
// The directory matches, the files in it don't: keep it.
assert!(!exclude.match_dir(b"/p/foo.log").matches_all_below());
```

The behaviorally identical JS twin is [`@globstar/core`]; the
filesystem walker built on this crate is [`globstar-walk`]. Dialect
spec, theory notes, and the shared golden corpus live in the
[repository](https://github.com/shulaoda/globstar).

[`@globstar/core`]: https://www.npmjs.com/package/@globstar/core
[`globstar-walk`]: https://crates.io/crates/globstar-walk
