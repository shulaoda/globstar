//! Tier 0 — pure literal matcher, used when the parsed pattern has no
//! metacharacters. A byte-by-byte compare with path-separator
//! normalization (GLOB_SPEC §12.3): each pattern `/` consumes exactly one
//! separator byte (`\` too on Windows), and `//` is not collapsed, so
//! `a//b` and `a/b` stay distinct (matches picomatch / bash). No
//! allocation per `is_match`.
//!
//! Helpers are const-generic over `CI`, monomorphized into two branch-free
//! loops (case-sensitive / case-insensitive).

use crate::dir_match::DirMatch;
use crate::engine::eq_byte;

/// Compiled Tier 0 matcher: a single literal byte sequence.
#[derive(Debug, Clone)]
pub struct LiteralMatcher {
    pub(crate) literal: Vec<u8>,
    /// ASCII case-insensitive compares when `true`; non-ASCII bytes still
    /// compare verbatim.
    pub(crate) case_insensitive: bool,
}

impl LiteralMatcher {
    pub fn new(literal: Vec<u8>, case_insensitive: bool) -> Self {
        Self {
            literal,
            case_insensitive,
        }
    }

    /// Whether `path` matches the compiled literal (§12.3 normalization).
    #[inline(always)]
    pub fn is_match(&self, path: &[u8]) -> bool {
        if self.case_insensitive {
            path_eq::<true>(&self.literal, path)
        } else {
            path_eq::<false>(&self.literal, path)
        }
    }

    /// Compute `match_dir` for a pure-literal pattern.
    ///
    /// - `dir_path` equals the literal → [`DirMatch::Match`]
    /// - literal starts with `dir_path + "/"` → [`DirMatch::Descend`]
    /// - otherwise → [`DirMatch::Pruned`]
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        if self.case_insensitive {
            match_dir_inner::<true>(&self.literal, dir_path)
        } else {
            match_dir_inner::<false>(&self.literal, dir_path)
        }
    }
}

/// Whole-path equality with separator normalization (§12.3). `CI=true`
/// uses ASCII case-insensitive byte equality.
#[inline]
pub(crate) fn path_eq<const CI: bool>(literal: &[u8], path: &[u8]) -> bool {
    let mut lit_i = 0usize;
    let mut path_i = 0usize;
    while lit_i < literal.len() && path_i < path.len() {
        let lb = literal[lit_i];
        let pb = path[path_i];
        if lb == b'/' && std::path::is_separator(pb as char) {
            // Strict separator: pattern's `/` consumes exactly one
            // separator byte from the path, not a run.
            lit_i += 1;
            path_i += 1;
        } else if eq_byte::<CI>(lb, pb) {
            lit_i += 1;
            path_i += 1;
        } else {
            return false;
        }
    }
    lit_i == literal.len() && path_i == path.len()
}

/// Whether the literal lives strictly under `dir_path`, with separator
/// normalization. An empty `dir_path` is treated as the cwd, so anything
/// is "under" it.
#[inline]
fn literal_under<const CI: bool>(literal: &[u8], dir_path: &[u8]) -> bool {
    if dir_path.is_empty() {
        return true;
    }
    let mut lit_i = 0usize;
    let mut dir_i = 0usize;
    while lit_i < literal.len() && dir_i < dir_path.len() {
        let lb = literal[lit_i];
        let db = dir_path[dir_i];
        if lb == b'/' && std::path::is_separator(db as char) {
            // Strict separator: exactly one separator byte per `/`.
            lit_i += 1;
            dir_i += 1;
        } else if eq_byte::<CI>(lb, db) {
            lit_i += 1;
            dir_i += 1;
        } else {
            return false;
        }
    }
    // dir_path fully consumed; literal still has more, starting with `/`.
    dir_i == dir_path.len() && lit_i < literal.len() && literal[lit_i] == b'/'
}

/// Const-generic body for [`LiteralMatcher::match_dir`]. Short-circuits
/// on a `path_eq` hit so `literal_under` is only called when the path
/// is not the literal itself.
#[inline]
fn match_dir_inner<const CI: bool>(literal: &[u8], dir_path: &[u8]) -> DirMatch {
    if path_eq::<CI>(literal, dir_path) {
        return DirMatch::Match;
    }
    if literal_under::<CI>(literal, dir_path) {
        return DirMatch::Descend;
    }
    DirMatch::Pruned
}
