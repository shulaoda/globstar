use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Options of a [`Walk`](crate::Walk). For a base and one change,
/// update [`WalkOptions::new`]:
///
/// ```
/// use globstar_walk::WalkOptions;
///
/// let opts = WalkOptions { dot: true, ..WalkOptions::new("./src") };
/// # let _ = opts;
/// ```
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// The folder to walk; default `.`. Made absolute at construction
    /// the way Node's `path.resolve` does: against the current
    /// directory, `.` and `..` taken out as text (so `jump/..` is the
    /// folder holding `jump` even when `jump` is a symlink), no symlink
    /// resolved. A later `set_current_dir` does not move the walk.
    pub base: PathBuf,

    /// Whether `*`, `?` and negated classes can take a leading `.`;
    /// default `false`.
    pub dot: bool,

    /// ASCII case-insensitive matching of patterns and ignore entries;
    /// default `false`.
    ///
    /// A seed is the pattern's literal head as written. On a
    /// case-sensitive file system a folder spelled in another case is
    /// not reached, though the matcher would accept it; write the head
    /// in the on-disk case, or as a class (`[Ss]rc/`). On a
    /// case-insensitive file system (macOS, Windows) the seed resolves
    /// through the file system's own folding and the yielded paths
    /// carry the pattern's case, so an ignore entry does not hold
    /// against a pattern that spells its head in another case
    /// (`.GIT/config` reaches `.git/config` past `**/.git/**`): walk
    /// such a file system with `true` when the ignore list must hold
    /// against patterns from others.
    pub case_insensitive: bool,

    /// Follow symlinks; default `true`. A link to a folder already
    /// reached through a link on the way down is not descended (see the
    /// [crate docs](crate#symlinks)); a folder that links reach by
    /// several routes is read once per route, so walk a tree that
    /// others control with `false`, which drops symlinks entirely. An
    /// entry is judged when its folder is read, so `false` is no guard
    /// against a folder replaced by a link during the walk.
    pub follow_links: bool,

    /// Entries matching any of these are skipped, and folders pruned;
    /// default empty.
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

// Specific impls: a blanket `impl<P: AsRef<Path>>` would conflict with
// core's `impl<T> From<T> for T`.
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
