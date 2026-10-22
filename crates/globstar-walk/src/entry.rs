use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// A single entry yielded by [`Walk`](crate::Walk): its absolute path,
/// the [`FileType`](std::fs::FileType) cached at readdir, and its depth
/// below `base`.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub(crate) path: PathBuf,
    pub(crate) file_type: std::fs::FileType,
    pub(crate) is_dir: bool,
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
    /// Does not follow symlinks.
    ///
    /// `FileType` already exposes `.is_file()` and `.is_symlink()`, which
    /// this crate does not repeat on `DirEntry`; [`is_dir`](Self::is_dir)
    /// is kept because it differs: it is true for a symlink to a folder.
    pub fn file_type(&self) -> std::fs::FileType {
        self.file_type
    }

    /// Whether the walk took this entry for a folder: a folder, or a
    /// symlink to one (where [`file_type`](Self::file_type) says
    /// symlink). No syscall.
    pub fn is_dir(&self) -> bool {
        self.is_dir
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
    /// On Windows the metadata the readdir (for a seed, its `lstat`) gave
    /// is returned without any syscall. On Unix this issues one
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
