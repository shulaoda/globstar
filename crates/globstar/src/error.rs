use core::fmt;

pub const MAX_PATTERN_LEN: usize = 64 * 1024;
pub const MAX_BRACE_NESTING: usize = 32;
/// What distributing braces around `**` may copy (§7.7), per pattern or
/// per union: one per token, a literal or a class by its length, and one
/// per brace branch.
pub const MAX_EXPANSION: usize = 4096;

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
    /// Escaped path separator (`\/`). A `/` can never appear inside a file
    /// name on any platform, so the escape has no possible match.
    EscapedSeparator { at: usize },
    /// Brace nesting exceeds [`MAX_BRACE_NESTING`], in the pattern or once
    /// braces are distributed around `**`.
    BraceNestingTooDeep { max: usize },
    /// Distributing braces around `**` would copy more than
    /// [`MAX_EXPANSION`].
    BraceExpansionTooLarge { max: usize },
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
        }
    }
}

impl std::error::Error for GlobError {}

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
