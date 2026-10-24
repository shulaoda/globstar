//! Errors of [`Walk`](crate::Walk).

use std::io;
use std::path::PathBuf;

/// `InvalidPattern` comes from construction, and so does `Io` when
/// `base` cannot be made absolute (no current directory). Every other
/// `Io` is yielded in place while the walk goes on, so a consumer can
/// log it and continue; the base check and the seeds' stats run on the
/// first `next()`.
#[derive(Debug)]
#[non_exhaustive]
pub enum WalkError {
    /// A pattern or ignore entry failed to compile.
    InvalidPattern {
        /// The pattern as written (`!src/[z-a]`); in a list, the member
        /// that failed. One longer than
        /// [`MAX_PATTERN_LEN`](globstar::error::MAX_PATTERN_LEN) bytes
        /// is named by its first 64 characters and `…`.
        pattern: String,
        /// The parser's reason. When the walker read the pattern
        /// differently (a leading `!` or `/` gone, `//` or `.` taken
        /// out), it ends with the text its byte offsets count in:
        /// `(in 'src/[z-a]', as the walker reads it)`.
        reason: String,
        /// The compile error (also
        /// [`Error::source`](std::error::Error::source)); `None` for an
        /// entry the walker refuses itself, an ignore entry that starts
        /// with `!`.
        source: Option<globstar::GlobError>,
    },

    /// The base is missing or not a folder, a seed's part or a folder
    /// could not be read, or an entry could not be typed. A folder
    /// removed or made a file after its parent listed it is left out,
    /// not an error.
    Io {
        /// The folder being read, or the path that failed.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPattern {
                pattern, reason, ..
            } => {
                write!(f, "invalid pattern '{pattern}': {reason}")
            }
            Self::Io { path, source } => {
                write!(f, "walker error at {}: {}", path.display(), source)
            }
        }
    }
}

impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPattern { source, .. } => source
                .as_ref()
                .map(|e| e as &(dyn std::error::Error + 'static)),
            Self::Io { source, .. } => Some(source),
        }
    }
}
