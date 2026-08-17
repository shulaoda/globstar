# globstar-walk

Filesystem walker powered by [`globstar`]. Static-prefix jump-start,
`match_dir` subtree pruning, symlink cycle breaking, `!`-pattern
auto-split into the ignore set.

```rust
use globstar_walk::Walk;

for entry in Walk::new("src/**/*.rs", ".")? {
    println!("{}", entry?.path().display());
}
```

The JS twin is [`@globstar/walk`]. Specs and the shared corpus live in
the [repository](https://github.com/shulaoda/globstar).

[`globstar`]: https://crates.io/crates/globstar
[`@globstar/walk`]: https://www.npmjs.com/package/@globstar/walk
