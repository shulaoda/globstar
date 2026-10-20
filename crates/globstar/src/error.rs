//! Compile errors, and the limits that guard compiling.

use core::fmt;

/// The longest pattern accepted, in bytes ([`GlobError::TooLong`]).
pub const MAX_PATTERN_LEN: usize = 64 * 1024;
/// How deep braces may nest ([`GlobError::BraceNestingTooDeep`]).
pub const MAX_BRACE_NESTING: usize = 32;
/// What distributing braces around `**` may copy (§6.2), per pattern or
/// per union: one per token, a literal or a class by its length, and one
/// per brace branch.
pub const MAX_EXPANSION: usize = 4096;

/// Why a pattern does not compile. An `at` field is a byte offset into
/// the UTF-8 pattern; a `max` field is the limit that was passed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GlobError {
    /// Empty pattern (`""`).
    Empty,
    /// Pattern length exceeds [`MAX_PATTERN_LEN`].
    TooLong {
        /// The pattern's length in bytes.
        len: usize,
        /// [`MAX_PATTERN_LEN`].
        max: usize,
    },
    /// Unterminated character class `[...`.
    UnterminatedClass {
        /// Where the class opens.
        at: usize,
    },
    /// Unterminated brace `{...`.
    UnterminatedBrace {
        /// Where the brace opens.
        at: usize,
    },
    /// Pattern ends with a lone backslash.
    TrailingBackslash,
    /// Escaped path separator (`\/`). A `/` can never appear inside a file
    /// name on any platform, so the escape has no possible match.
    EscapedSeparator {
        /// Where the `\` is.
        at: usize,
    },
    /// Brace nesting exceeds [`MAX_BRACE_NESTING`], in the pattern or once
    /// braces are distributed around `**`.
    BraceNestingTooDeep {
        /// [`MAX_BRACE_NESTING`].
        max: usize,
    },
    /// Distributing braces around `**` would copy more than
    /// [`MAX_EXPANSION`].
    BraceExpansionTooLarge {
        /// [`MAX_EXPANSION`].
        max: usize,
    },
    /// Compiling would take more than `max` steps of work: braces whose
    /// segments multiply (`a{a,/}` repeated many times). `max` grows with
    /// the pattern, so every pattern of ordinary shape compiles. A list
    /// given to `Glob::union` compiles as one, so this is the list's
    /// error (never inside [`InMember`](Self::InMember)), and members
    /// that each compile alone can fail together.
    TooComplex {
        /// The work this pattern was allowed.
        max: usize,
    },
    /// Character class range with right endpoint smaller than left.
    InvalidRange {
        /// Where the range starts (its first end).
        at: usize,
        /// The first end, as a byte.
        low: u8,
        /// The second end, as a byte.
        high: u8,
    },
    /// `Glob::union` was called with an empty iterator.
    EmptyPatternSet,
    /// A negated pattern (an odd number of leading `!`) was passed to
    /// `Glob::union`.
    NegatedInUnion {
        /// Its position in the list, counting every member given.
        index: usize,
        /// The pattern as given.
        pattern: String,
    },
    /// A member of the list given to `Glob::union` does not compile:
    /// `error` says why, with offsets into that member. A failure of the
    /// list as a whole ([`TooComplex`](Self::TooComplex)) is not wrapped.
    InMember {
        /// The member's position in the list, counting every member given.
        index: usize,
        /// Why it does not compile.
        error: Box<GlobError>,
    },
}

impl GlobError {
    /// The error itself, past any [`InMember`](Self::InMember) wrapping:
    /// what to match on to tell the kind of a union's failure.
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
            Self::BraceExpansionTooLarge { max } => {
                write!(f, "brace expansion around `**` exceeds limit {max}")
            }
            Self::TooComplex { max } => {
                write!(
                    f,
                    "pattern too complex: compiling it takes more than {max} steps"
                )
            }
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

/// A byte as the user wrote it, or as `\xHH` when it is not a printable
/// ASCII character (a class range is byte-based, so this may be one byte
/// of a multi-byte character).
fn show(byte: u8) -> String {
    if byte.is_ascii_graphic() {
        (byte as char).to_string()
    } else {
        format!("\\x{byte:02x}")
    }
}
