//! Errors produced when compiling a glob pattern.
//!
//! Variants correspond 1:1 to GLOB_SPEC.md §10.

use core::fmt;

/// Maximum allowed pattern length in bytes (defense against pathological input).
pub const MAX_PATTERN_LEN: usize = 64 * 1024;

/// Maximum allowed brace nesting depth.
pub const MAX_BRACE_NESTING: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobError {
    /// Empty pattern (`""`).
    Empty,
    /// Pattern length exceeds [`MAX_PATTERN_LEN`].
    TooLong { len: usize, max: usize },
    /// Unterminated character class `[...`.
    UnterminatedClass { at: usize },
    /// Unterminated brace `{...`.
    UnterminatedBrace { at: usize },
    /// Pattern ends with a lone backslash.
    TrailingBackslash,
    /// Brace nesting exceeds [`MAX_BRACE_NESTING`].
    BraceNestingTooDeep { max: usize },
    /// Character class range with right endpoint smaller than left.
    InvalidRange { at: usize, low: u8, high: u8 },
    /// `Glob::union` was called with an empty iterator.
    EmptyPatternSet,
    /// A negated (`!`-prefixed) pattern was passed to `Glob::union`.
    NegatedInUnion { index: usize, pattern: String },
}

impl fmt::Display for GlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty pattern"),
            Self::TooLong { len, max } => write!(f, "pattern too long: {len} > {max}"),
            Self::UnterminatedClass { at } => {
                write!(f, "unterminated character class at byte {at}")
            }
            Self::UnterminatedBrace { at } => {
                write!(f, "unterminated brace expansion at byte {at}")
            }
            Self::TrailingBackslash => write!(f, "pattern ends with lone backslash"),
            Self::BraceNestingTooDeep { max } => write!(f, "brace nesting exceeds limit {max}"),
            Self::InvalidRange { at, low, high } => write!(
                f,
                "invalid character class range {low}..{high} at byte {at}"
            ),
            Self::EmptyPatternSet => write!(f, "Glob::union requires at least one pattern"),
            Self::NegatedInUnion { index, pattern } => write!(
                f,
                "negated pattern {pattern:?} at index {index} is not allowed in Glob::union; \
                 use Glob::union(includes) and Glob::union(excludes) separately"
            ),
        }
    }
}

impl std::error::Error for GlobError {}
