use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// An entry [`Walk`](crate::Walk) yielded: its absolute path, the
/// [`FileType`](std::fs::FileType) the listing gave, and its depth below
/// `base`.
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

    /// The entry's absolute path, owned.
    pub fn into_path(self) -> PathBuf {
        self.path
    }

    /// The last component of the path; the whole path when it has none
    /// (the root `/`), as `walkdir` does.
    pub fn file_name(&self) -> &OsStr {
        self.path
            .file_name()
            .unwrap_or_else(|| self.path.as_os_str())
    }

    /// The type the listing gave, symlinks not followed; no syscall.
    /// `FileType` has `is_file` and `is_symlink`; [`is_dir`](Self::is_dir)
    /// is kept here because it differs.
    pub fn file_type(&self) -> std::fs::FileType {
        self.file_type
    }

    /// Whether the walk took the entry for a folder: a folder, or a
    /// symlink to one. No syscall.
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// Depth below `base`, whose own children are depth 1.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The entry's metadata, symlinks not followed (as
    /// [`std::fs::DirEntry::metadata`]). On Windows the listing's, with
    /// no syscall; elsewhere one `symlink_metadata` per call.
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
