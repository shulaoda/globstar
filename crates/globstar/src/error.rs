//! Compile errors and the limits that guard compiling.

use core::fmt;

/// The longest pattern accepted, in bytes.
pub const MAX_PATTERN_LEN: usize = 64 * 1024;
/// How deep braces may nest.
pub const MAX_BRACE_NESTING: usize = 32;

/// Why a pattern does not compile. An `at` is a byte offset into the
/// UTF-8 pattern; a `max` is the limit that was passed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GlobError {
    /// The empty pattern.
    Empty,
    /// Longer than [`MAX_PATTERN_LEN`].
    TooLong {
        /// The pattern's length in bytes.
        len: usize,
        /// [`MAX_PATTERN_LEN`].
        max: usize,
    },
    /// A class without its `]`.
    UnterminatedClass {
        /// Where the class opens.
        at: usize,
    },
    /// A brace without its `}`.
    UnterminatedBrace {
        /// Where the brace opens.
        at: usize,
    },
    /// The pattern ends with a lone `\`.
    TrailingBackslash,
    /// `\/`: no file name holds a `/`, so the escape could never match.
    EscapedSeparator {
        /// Where the `\` is.
        at: usize,
    },
    /// Braces nested deeper than [`MAX_BRACE_NESTING`].
    BraceNestingTooDeep {
        /// [`MAX_BRACE_NESTING`].
        max: usize,
    },
    /// A class range whose end is below its start.
    InvalidRange {
        /// Where the range starts.
        at: usize,
        /// The first end, as a byte.
        low: u8,
        /// The second end, as a byte.
        high: u8,
    },
    /// `Glob::union` of no patterns.
    EmptyPatternSet,
    /// A negated pattern given to `Glob::union`.
    NegatedInUnion {
        /// Its position in the list, counting every member given.
        index: usize,
        /// The pattern as given.
        pattern: String,
    },
    /// A member of a `Glob::union` list does not compile.
    InMember {
        /// The member's position in the list, counting every member given.
        index: usize,
        /// Why it does not compile, with offsets into that member.
        error: Box<GlobError>,
    },
}

impl GlobError {
    /// The error past any [`InMember`](Self::InMember) wrapping.
    pub fn root(&self) -> &GlobError {
        match self {
            Self::InMember { error, .. } => error.root(),
            e => e,
        }
    }
}

impl fmt::Display for GlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty pattern"),
            Self::TooLong { len, max } => write!(f, "pattern too long: {len} > {max}"),
            Self::UnterminatedClass { at } => write!(
                f,
                "unterminated character class at byte {at}: no `]` before a `/` or the end of \
                 the pattern"
            ),
            Self::UnterminatedBrace { at } => {
                write!(f, "unterminated brace expansion at byte {at}")
            }
            Self::TrailingBackslash => write!(f, "pattern ends with lone backslash"),
            Self::EscapedSeparator { at } => {
                write!(f, "escaped separator `\\/` at byte {at}")
            }
            Self::BraceNestingTooDeep { max } => write!(f, "brace nesting exceeds limit {max}"),
            Self::InvalidRange { at, low, high } => write!(
                f,
                "invalid character class range `{}-{}` at byte {at}: the end is below the start",
                show(*low),
                show(*high)
            ),
            Self::EmptyPatternSet => write!(f, "Glob::union requires at least one pattern"),
            Self::NegatedInUnion { index, pattern } => write!(
                f,
                "negated pattern {pattern:?} at index {index} is not allowed in Glob::union: \
                 drop the `!` and keep the excludes in a union of their own"
            ),
            Self::InMember { index, error } => write!(f, "pattern {index}: {error}"),
        }
    }
}

impl std::error::Error for GlobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InMember { error, .. } => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// A byte as written, or as `\xHH` when it is not printable ASCII.
fn show(byte: u8) -> String {
    if byte.is_ascii_graphic() {
        (byte as char).to_string()
    } else {
        format!("\\x{byte:02x}")
    }
}
