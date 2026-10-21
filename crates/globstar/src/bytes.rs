//! Byte primitives the engines share: separators, ASCII case folding and
//! the comparisons built on them. JS twin: `packages/globstar/src/bytes.js`.

use std::cmp::Ordering;

/// A path separator: `/`, and `\` on Windows (GLOB_SPEC §2.2).
#[inline(always)]
pub(crate) fn is_sep(b: u8) -> bool {
    std::path::is_separator(char::from(b))
}

/// Does the literal `lit` hold an escaped `\`? On Windows that is a
/// separator no path segment holds, so the literal matches nothing
/// (§4.2); elsewhere it is a plain byte.
pub(crate) fn has_escaped_sep(lit: &[u8]) -> bool {
    cfg!(windows) && lit.contains(&b'\\')
}

/// `b`, ASCII-lowercased when case-insensitive.
#[inline(always)]
pub(crate) fn fold(b: u8, ci: bool) -> u8 {
    if ci { b.to_ascii_lowercase() } else { b }
}

/// A pattern byte against a path byte: `/` takes any separator, a
/// separator byte of the pattern (an escaped `\` on Windows) takes
/// nothing (§2.2), and letters fold when `ci`.
#[inline(always)]
pub(crate) fn pat_byte_eq(pat: u8, b: u8, ci: bool) -> bool {
    if pat == b'/' {
        is_sep(b)
    } else if is_sep(pat) {
        false
    } else if ci {
        pat.eq_ignore_ascii_case(&b)
    } else {
        pat == b
    }
}

/// Two runs of one length, equal but for ASCII case when `ci`. Byte-wise,
/// so short ones stay inline rather than calling `bcmp`.
#[inline(always)]
pub(crate) fn affix_eq(part: &[u8], seg_part: &[u8], ci: bool) -> bool {
    debug_assert_eq!(part.len(), seg_part.len());
    if ci {
        part.iter()
            .zip(seg_part)
            .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
    } else {
        part.iter().zip(seg_part).all(|(a, b)| a == b)
    }
}

/// Does `path` end with the pattern bytes `suffix` (`pat_byte_eq`)?
/// Last byte first: that is where extensions differ.
#[inline]
pub(crate) fn ends_with_pattern(path: &[u8], suffix: &[u8], ci: bool) -> bool {
    path.len() >= suffix.len()
        && suffix
            .iter()
            .zip(&path[path.len() - suffix.len()..])
            .rev()
            .all(|(&s, &p)| pat_byte_eq(s, p, ci))
}

/// A literal against path bytes in the order sorted literals are kept:
/// bytes as they are, unless case-insensitive (ASCII folded) or on
/// Windows (a path's separators as `/`).
pub(crate) fn cmp_folded(lit: &[u8], path: &[u8], ci: bool) -> Ordering {
    if !ci && !cfg!(windows) {
        return lit.cmp(path);
    }
    let path_byte = |&b: &u8| if is_sep(b) { b'/' } else { fold(b, ci) };
    lit.iter()
        .map(|&b| fold(b, ci))
        .cmp(path.iter().map(path_byte))
}

/// Index just past the segment that starts at `pos`.
#[inline]
pub(crate) fn seg_end(path: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i < path.len() && !is_sep(path[i]) {
        i += 1;
    }
    i
}

/// Start of the segment that ends at `end`.
#[inline]
pub(crate) fn seg_start(path: &[u8], end: usize) -> usize {
    let mut s = end;
    while s > 0 && !is_sep(path[s - 1]) {
        s -= 1;
    }
    s
}
