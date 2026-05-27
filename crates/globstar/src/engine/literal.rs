//! Tier 0 — pure literal matcher.
//!
//! For patterns that contain no metacharacters (after parse). Implementation
//! is a single byte comparison with path-separator normalization.
//!
//! The literal-facts pre-filter for other tiers lives in [`super::facts`].
//!
//! ## Path separator handling (GLOB_SPEC §12.3)
//!
//! `\` is always an escape on the pattern side (never a separator). On the
//! match side we treat any [`std::path::is_separator`] byte (`/` always, `\`
//! on Windows only — the `Seps` set from §12.3) as a separator. Each `/` in
//! the pattern consumes **exactly one** separator byte from the path —
//! aligned with `picomatch` / `globset` / `bash` / `fast-glob`. Pattern-side
//! `//` is NOT collapsed: `a//b` and `a/b` are distinct patterns. Paths
//! with redundant separator runs (`a//b`) are not silently equated to the
//! canonical form; callers that want lenient handling should normalize
//! before matching.
//!
//! These rules are applied **byte-by-byte without allocation**, so a single
//! `is_match` call has no heap traffic.
//!
//! ## case_insensitive specialization
//!
//! Each helper is generic over `const CI: bool`. The compiler monomorphizes
//! into two specialized functions where `CI` is a compile-time constant —
//! the `if CI { ... } else { ... }` choice in the byte-equality helper
//! ([`super::eq_byte`]) is dead-code-eliminated per instantiation, leaving
//! a tight single-mode loop with zero per-byte branch on case mode.
//!
//! `is_match` / `match_dir` dispatch inline on `case_insensitive`.
//! No `#[cold]` wrapper on the CI side: Tier 0 patterns rarely
//! dominate hot paths (single `Glob::new(literal)` calls are
//! uncommon; the realistic high-volume case is many literal entries
//! merged via `Glob::union` into one DFA where heap savings, not
//! micro-branch prediction, are the win).

use crate::dir_match::DirMatch;
use crate::engine::eq_byte;

/// Compiled Tier 0 matcher: a single literal byte sequence.
#[derive(Debug, Clone)]
pub struct LiteralMatcher {
    pub(crate) literal: Vec<u8>,
    /// ASCII case-insensitive compare flag — propagated from
    /// [`crate::options::CompileOptions`]. When `true`, byte compares use
    /// `eq_ignore_ascii_case`; non-ASCII bytes still compare verbatim.
    pub(crate) case_insensitive: bool,
}

impl LiteralMatcher {
    pub fn new(literal: Vec<u8>, case_insensitive: bool) -> Self {
        Self {
            literal,
            case_insensitive,
        }
    }

    /// Whether `path` matches the compiled literal (per §12.3 normalization).
    ///
    /// Dispatches to a const-generic monomorphization of [`path_eq`];
    /// `#[inline(always)]` keeps the chosen body inside `Glob::is_match`.
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

/// Compare a parser-normalized literal against a raw path with
/// separator-equivalent semantics (§12.3). `CI=true` switches to
/// ASCII case-insensitive byte equality via the const-folded
/// [`eq_byte`].
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
