# Rust API

The `globstar` crate exposes one type for a compiled pattern or pattern set, `Glob`, with its options, the `DirMatch` verdict and the `GlobError` enum. Paths are `&[u8]`: the pattern's UTF-8 bytes are what match, and a path is matched byte by byte with no decoding step. The crate is `#![forbid(unsafe_code)]` and has no dependencies.

```rust
use globstar::{CompileOptions, DirMatch, Glob, GlobError, Matcher};
```

## `Glob`

```rust
impl Glob {
    pub fn new(pattern: &str) -> Result<Glob, GlobError>;
    pub fn new_with(pattern: &str, opts: CompileOptions) -> Result<Glob, GlobError>;
    pub fn union<I, S>(patterns: I) -> Result<Glob, GlobError>
    where I: IntoIterator<Item = S>, S: AsRef<str>;
    pub fn union_with<I, S>(patterns: I, opts: CompileOptions) -> Result<Glob, GlobError>
    where I: IntoIterator<Item = S>, S: AsRef<str>;

    pub fn is_match(&self, path: &[u8]) -> bool;
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch;
    pub fn static_prefixes(&self) -> Vec<Vec<u8>>;
}
```

`Glob` is `Debug + Clone + Send + Sync`; compile once and share it across threads (its `Debug` shows the engine and the negation, not the compiled tables). A union is an ordinary `Glob`, not a separate set type. `union` rejects a negated member (an odd number of leading `!`; `!!x` is just `x`) with `GlobError::NegatedInUnion`, an empty iterator with `GlobError::EmptyPatternSet`, and a member that does not compile with `GlobError::InMember`, which names it by index; a pattern given twice counts once.

```rust
let glob = Glob::new("src/**/*.rs")?;
assert!(glob.is_match(b"src/engine/mod.rs"));

let set = Glob::union(["src/**/*.rs", "docs/**/*.md"])?;
assert!(set.is_match(b"docs/guide/intro.md"));
assert_eq!(set.static_prefixes(), vec![b"src".to_vec(), b"docs".to_vec()]);
```

## `CompileOptions`

```rust
#[non_exhaustive]
pub struct CompileOptions {
    pub dot: bool, // default true
    pub case_insensitive: bool, // default false
}

impl CompileOptions {
    pub fn dot(self, v: bool) -> Self;
    pub fn case_insensitive(self, v: bool) -> Self;
}
```

```rust
let opts = CompileOptions::default().dot(false);
let glob = Glob::new_with("*", opts)?;
assert!(!glob.is_match(b".env"));
```

`dot: false` stops every wildcard from matching a leading `.`; the `globstar-walk` crate defaults to it. `case_insensitive` folds ASCII letters only. These are the only two options the dialect has.

## `DirMatch`

```rust
#[non_exhaustive]
pub enum DirMatch {
    Match,              // the directory matches; nothing below it can
    Pruned,             // neither the directory nor anything below matches
    Descend,            // the directory does not match; something below may
    DescendAndMatch,    // the directory matches, and something below may
    DescendAll,         // the directory does not match; everything below does
    DescendAllAndMatch, // the directory matches, and so does everything below
}

impl DirMatch {
    pub fn new(matches: bool, may_below: bool, all_below: bool) -> Self;
    pub fn matches(self) -> bool;
    pub fn may_match_below(self) -> bool;
    pub fn matches_all_below(self) -> bool;
}
```

The names read from the pattern's side; a walker reads the three facts according to the role the pattern plays. A pattern that selects files walks into a directory while `may_match_below()` holds. A pattern that excludes files skips a directory whole once `matches_all_below()` holds, and needs no more tests below it once `may_match_below()` is `false`. See [Directory matching](../guide/directory-matching.md).

```rust
let exclude = Glob::union(["**/node_modules/**", "**/*.log"])?;
// Everything below is excluded: skip the directory whole.
assert!(exclude.match_dir(b"/p/node_modules").matches_all_below());
// The directory matches, the files in it don't: keep it.
assert!(!exclude.match_dir(b"/p/foo.log").matches_all_below());
```

## `Matcher`

```rust
pub trait Matcher {
    fn is_match(&self, path: &[u8]) -> bool;
    fn match_dir(&self, dir_path: &[u8]) -> DirMatch;
    fn static_prefixes(&self) -> Vec<Vec<u8>>;
}
```

The matching surface as a trait, implemented by `Glob` (`globstar-walk` uses `Glob` directly). It is object-safe, so a caller can hold `Box<dyn Matcher>` when it mixes matcher sources.

## `GlobError`

```rust
#[non_exhaustive]
pub enum GlobError {
    Empty,
    TooLong { len: usize, max: usize },
    UnterminatedClass { at: usize },
    UnterminatedBrace { at: usize },
    TrailingBackslash,
    EscapedSeparator { at: usize },
    BraceNestingTooDeep { max: usize },
    InvalidRange { at: usize, low: u8, high: u8 },
    EmptyPatternSet,
    NegatedInUnion { index: usize, pattern: String },
    InMember { index: usize, error: Box<GlobError> },
}
```

`GlobError` implements `Display` and `std::error::Error` (`InMember`'s `source()` is the error inside; `root()` reads through it). The enum is `#[non_exhaustive]`: match it with a `_` arm. The limits are public constants in `globstar::error`: `MAX_PATTERN_LEN` (64 KiB) and `MAX_BRACE_NESTING` (32). What each variant means, with an example, is in [Errors and limits](../guide/errors.md).

```rust
match Glob::new("src/[a") {
    Err(GlobError::UnterminatedClass { at }) => eprintln!("class opened at byte {at} never closes"),
    Err(e) => eprintln!("{e}"),
    Ok(_) => unreachable!(),
}
```

## Walker

The filesystem walker is the `globstar-walk` crate: an iterator over matched entries that seeds from `static_prefixes`, prunes with `match_dir`, and takes a separate ignore set. Its options are the JS walker's under Rust names (`base` for `cwd`, `follow_links` for `followSymlinks`). It yields matching folders as well as files (the JS walker yields files only), never FIFOs, sockets or devices; a pattern or ignore entry that ends in `/` matches folders only, and an ignore entry that ends in `/` or `/**` prunes the folder before it is read. A folder that links reach by several routes is walked once per route, as in every walker that follows links, so walk a tree that others control (a cloned repository, an unpacked archive) with `follow_links: false`. A pattern's literal head is opened as written, so on a case-insensitive file system (macOS and Windows by default) it reaches names spelled in another case, past an ignore entry for the listed spelling (`.GIT/config` past `**/.git/**`): walk such a file system with `case_insensitive: true` when the ignore list must hold against patterns from others. Its behavior is governed by its own crate documentation.
