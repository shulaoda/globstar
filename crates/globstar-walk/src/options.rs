//! Construction-time options for [`Walk`](crate::Walk).
//!
//! Anything path-like converts to [`WalkOptions`] via [`From`], so
//! `Walk::new(pattern, "./src")` works without spelling out the
//! struct. For base + one tweak, use struct-update syntax on top of
//! [`WalkOptions::new`]:
//!
//! ```ignore
//! WalkOptions { dot: true, ..WalkOptions::new("./src") }
//! ```

use std::path::{Path, PathBuf};

/// Configuration for a [`Walk`](crate::Walk). See module docs for
/// construction ergonomics; per-field defaults are in the [`Default`]
/// impl.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Root directory to walk. Default: `.` (current working directory).
    ///
    /// Locked to an absolute path at [`Walk`](crate::Walk) construction via
    /// [`std::path::absolute`], so subsequent `std::env::set_current_dir`
    /// calls don't redirect the walker. Symlinks are **not** resolved
    /// (that's [`std::fs::canonicalize`]'s job) — pre-canonicalize the
    /// path yourself if you need that.
    pub base: PathBuf,
    /// Whether `*` / `?` / negated classes can consume a leading `.` at
    /// segment boundaries. Default: `false` (Unix-style dotfile protection).
    pub dot: bool,
    /// ASCII case-insensitive matching for main and ignore patterns.
    /// Default: `false`.
    ///
    /// Note: the walker's `static_prefixes` jump-start still uses the
    /// literal prefix verbatim, so on case-insensitive filesystems where
    /// the directory name's case differs from the pattern, the walker may
    /// miss the seek. Workaround: normalize the pattern to match the
    /// filesystem case, or write `[Ss]rc/...` for the prefix portion.
    pub case_insensitive: bool,
    /// Follow symbolic links when descending. Cycles are detected via
    /// `fs::canonicalize` on the symlink's resolved target and skipped
    /// (the offending descent is dropped, not the entire walk). When
    /// `false`, symlinks are dropped entirely — neither emitted nor
    /// descended (matches `tinyglobby` / `fdir`'s `excludeSymlinks`).
    /// Default: `true`.
    pub follow_links: bool,
    /// Ignore patterns — entries matching any of these are skipped
    /// (files) or pruned (directories). Default: empty.
    pub ignore: Vec<String>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            base: PathBuf::from("."),
            dot: false,
            case_insensitive: false,
            follow_links: true,
            ignore: Vec::new(),
        }
    }
}

impl WalkOptions {
    /// Default options rooted at `base`.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            ..Self::default()
        }
    }
}

// Ergonomic `impl Into<WalkOptions>` conversions — so `Walk::new(pattern, "./src")`
// just works without wrapping. Specific `From` impls (rather than a blanket
// `impl<P: AsRef<Path>>`) avoid coherence conflicts with the reflexive
// `impl<T> From<T> for T` in core.
impl From<&str> for WalkOptions {
    fn from(base: &str) -> Self {
        Self::new(base)
    }
}
impl From<String> for WalkOptions {
    fn from(base: String) -> Self {
        Self::new(base)
    }
}
impl From<&Path> for WalkOptions {
    fn from(base: &Path) -> Self {
        Self::new(base)
    }
}
impl From<PathBuf> for WalkOptions {
    fn from(base: PathBuf) -> Self {
        Self::new(base)
    }
}
impl From<&PathBuf> for WalkOptions {
    fn from(base: &PathBuf) -> Self {
        Self::new(base.as_path())
    }
}
