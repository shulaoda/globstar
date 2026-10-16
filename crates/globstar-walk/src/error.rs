//! Error types produced by [`Walk`](crate::Walk).

use std::io;
use std::path::PathBuf;

/// Errors produced by [`Walk`](crate::Walk).
///
/// `InvalidPattern` surfaces at construction when any main or ignore
/// pattern fails to compile. `Io` errors are yielded lazily during
/// iteration — the base check and the seeds' stats too, which run on the
/// first `next()`; the walker continues after yielding an I/O error so
/// consumers can log and keep going.
#[derive(Debug)]
pub enum WalkError {
    /// A glob pattern (main or ignore) failed to parse/compile.
    InvalidPattern {
        /// The pattern that failed, as written (`!src/[z-a]`). When the
        /// members of a pattern list each compile alone but not together
        /// (they share one expansion budget, GLOB_SPEC §7.7), this is the
        /// whole list, formatted like `["a", "b"]`.
        pattern: String,
        /// Human-readable reason (from the underlying parser). When the
        /// walker read the pattern differently (a leading `!` or `/`
        /// gone, `//` or `.` taken out), it ends with the text its byte
        /// offsets count in: `(in 'src/[z-a]', as the walker reads it)`.
        reason: String,
    },

    /// An I/O error while reading a directory entry.
    Io {
        /// Path where the error occurred.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPattern { pattern, reason } => {
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
            Self::InvalidPattern { .. } => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}
