//! Compiled-engine module hierarchy.
//!
//! - `literal` — the pure-literal matcher.
//! - `segment` — the segment-structured matcher for every other
//!   pattern.

pub(crate) mod literal;
pub(crate) mod segment;

/// A pattern byte against a path byte: `/` takes any separator, a
/// separator byte of the pattern (an escaped `\` on Windows) takes
/// nothing (§2.2), and letters fold when `ci`.
#[inline(always)]
fn pat_byte_eq(pat: u8, b: u8, ci: bool) -> bool {
    if pat == b'/' {
        std::path::is_separator(char::from(b))
    } else if std::path::is_separator(char::from(pat)) {
        false
    } else if ci {
        pat.eq_ignore_ascii_case(&b)
    } else {
        pat == b
    }
}
