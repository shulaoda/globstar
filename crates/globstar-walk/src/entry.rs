//! Directory entries yielded by [`Walk`](crate::Walk).
//!
//! A thin, minimal analog of `walkdir::DirEntry` that fits our own
//! walker — we don't build on `walkdir`; we read directories via
//! `std::fs::read_dir` plus glob-aware pruning, and carry the bits we
//! pick up for free so consumers don't re-stat.
//!
//! Layout invariants:
//!
//! - **`file_type` is eagerly cached** at walk time. Recursive descent
//!   already needs to know "dir or not" to decide whether to descend, so
//!   preserving that [`FileType`](std::fs::FileType) is a zero-cost win
//!   — callers get `.is_file()` / `.is_dir()` / `.is_symlink()` through
//!   the standard library's `FileType` API without a syscall.
//! - **`depth` is tracked in the traversal frame**, not recomputed per
//!   entry. `base` is depth 0, its direct children are depth 1, etc.
//! - **`file_name` is lazily derived** from `path.file_name()` — matches
//!   walkdir's choice, saves ~64 bytes per entry (no extra `OsString`),
//!   and path-slice access is essentially free.
//! - **Windows metadata is eagerly cached** — `FindFirstFileW` returns
//!   attributes at readdir time, so we preserve them across the walker's
//!   ready buffer. On Unix, `stat` costs one syscall per call, so we
//!   stay lazy and only hit the filesystem when the user actually asks.
//!
//! The struct is deliberately slimmer than `walkdir::DirEntry` — no
//! `follow_link` flag (our `file_type` is always symlink-preserving,
//! so `entry.file_type().is_symlink()` is the canonical check) and no
//! Unix `ino` field (rarely needed by glob consumers; can be revisited).

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// A single entry yielded by [`Walk`](crate::Walk).
///
/// Holds the entry's absolute path (locked at walker construction), its
/// [`FileType`](std::fs::FileType) cached from the readdir call, and the
/// depth relative to the walker's base (`base` → 0, its direct children
/// → 1, and so on).
///
/// On Windows the full [`Metadata`](std::fs::Metadata) is also cached,
/// so [`DirEntry::metadata`] returns without any syscall. On Unix it's
/// lazy — a fresh `symlink_metadata` call on demand.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub(crate) path: PathBuf,
    pub(crate) file_type: std::fs::FileType,
    pub(crate) depth: usize,
    #[cfg(windows)]
    pub(crate) metadata: std::fs::Metadata,
}

impl DirEntry {
    /// The entry's absolute path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consume the entry and return the owned path.
    pub fn into_path(self) -> PathBuf {
        self.path
    }

    /// The file name of this entry — the last component of its path.
    ///
    /// If the path has no file name (e.g. it's the filesystem root `/`),
    /// the full path is returned as an `OsStr`. This matches
    /// `walkdir::DirEntry::file_name`.
    pub fn file_name(&self) -> &OsStr {
        self.path
            .file_name()
            .unwrap_or_else(|| self.path.as_os_str())
    }

    /// Cached [`FileType`](std::fs::FileType) for this entry — no syscall.
    ///
    /// `FileType` already exposes `.is_file()`, `.is_dir()`, and
    /// `.is_symlink()` via the standard library, so this crate doesn't
    /// duplicate those as direct methods on `DirEntry`.
    pub fn file_type(&self) -> std::fs::FileType {
        self.file_type
    }

    /// Depth below the walker's `base`. `base` itself is depth 0; its
    /// immediate children are depth 1, and so on.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Metadata for this entry. **Does not follow symlinks** — matches
    /// [`std::fs::DirEntry::metadata`]'s semantics. For follow-link
    /// semantics, call `std::fs::metadata(entry.path())` directly.
    ///
    /// On Windows the metadata is cached from the original readdir call
    /// and returned without any syscall. On Unix this issues one
    /// `symlink_metadata` syscall per invocation.
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        #[cfg(windows)]
        {
            Ok(self.metadata.clone())
        }
        #[cfg(not(windows))]
        {
            std::fs::symlink_metadata(&self.path)
        }
    }
}
