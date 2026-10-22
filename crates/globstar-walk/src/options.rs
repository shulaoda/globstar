use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Configuration for a [`Walk`](crate::Walk). For a base and one
/// change, update [`WalkOptions::new`]:
///
/// ```
/// use globstar_walk::WalkOptions;
///
/// let opts = WalkOptions { dot: true, ..WalkOptions::new("./src") };
/// # let _ = opts;
/// ```
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Root directory to walk. Default: `.` (current working directory).
    ///
    /// Locked to an absolute path at [`Walk`](crate::Walk) construction,
    /// the way Node's `path.resolve` does it: against the current
    /// directory (an empty base is the current directory itself), with
    /// `.` and `..` taken out as text, so `jump/..` is the folder holding
    /// `jump` even when `jump` is a symlink, as in the JS walker.
    /// Subsequent `std::env::set_current_dir` calls don't redirect the
    /// walker. Symlinks are **not** resolved (that's
    /// [`std::fs::canonicalize`]'s job) — pre-canonicalize the path
    /// yourself if you need that.
    pub base: PathBuf,

    /// Whether `*` / `?` / negated classes can consume a leading `.` at
    /// segment boundaries. Default: `false` (Unix-style dotfile protection).
    pub dot: bool,

    /// ASCII case-insensitive matching for main and ignore patterns.
    /// Default: `false`.
    ///
    /// Known limitation: static-prefix seeding uses the pattern's literal
    /// prefix verbatim. On a case-sensitive filesystem, a directory whose
    /// on-disk casing differs from the pattern is silently skipped even
    /// though the matcher accepts it. On case-insensitive filesystems the
    /// seed resolves through the filesystem's own folding and the walk
    /// works, with emitted paths carrying the pattern's casing — which
    /// also means that with `case_insensitive: false` on such a
    /// filesystem, a seeded literal prefix still folds through the
    /// filesystem (and case-variant brace prefixes can emit one file
    /// once per spelling). Workaround: write the prefix in the on-disk
    /// casing, or make it non-literal with a class like `[Ss]rc/...`.
    ///
    /// So on a case-insensitive filesystem (macOS and Windows by default)
    /// an ignore entry does not hold against a pattern that spells its
    /// literal head in another case: `.GIT/config` reaches `.git/config`
    /// past `**/.git/**`. Walk such a filesystem with `true` when the
    /// ignore list must hold against patterns from others.
    pub case_insensitive: bool,

    /// Follow symbolic links when descending. A link to a folder the walk
    /// already reached through a link on the way down (a cycle) is not
    /// descended; the rest of the walk goes on. Folders are told apart by `(dev, ino)` on Unix, confirmed
    /// by canonical path, and by canonical path on Windows. A folder that
    /// links reach by several routes is read once per route, so walk a
    /// tree that others control with `false` (see the
    /// [crate docs](crate#symlinks)). When
    /// `false`, symlinks are dropped entirely — neither emitted nor
    /// descended (matches `tinyglobby` / `fdir`'s `excludeSymlinks`).
    /// Each entry is judged when its folder is read: a folder replaced by
    /// a symlink after that is read through the link, so `false` is no
    /// guard against a tree that changes during the walk.
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
impl From<&String> for WalkOptions {
    fn from(base: &String) -> Self {
        Self::new(base.as_str())
    }
}
impl From<&OsStr> for WalkOptions {
    fn from(base: &OsStr) -> Self {
        Self::new(base)
    }
}
impl From<OsString> for WalkOptions {
    fn from(base: OsString) -> Self {
        Self::new(base)
    }
}
