//! Suffix-anchored pre-filter consulted before every engine's `is_match`.
//!
//! Every [`OpProgram`](super::ops::OpProgram) carries a [`LiteralFacts`]
//! recording the byte suffix (or a set of them, for a trailing brace
//! alternation) every matching path must end with. The matcher
//! short-circuits on a separator-aware `ends_with` before running the
//! engine:
//!
//! ```text
//! path ends with suffix  →  maybe match (run engine)
//!         otherwise       →  definitely not
//! ```
//!
//! On walker workloads this rejects the bulk of candidates outright —
//! `src/**/*.ts` drops every `.js` file in one suffix scan.
//!
//! ## Correctness invariant
//!
//! `accept(path) == false` ⇒ no program variant can match `path`, so the
//! filter must never reject a path the engine would accept. That drives:
//!
//! 1. **Conservative extraction** — stop at the first non-literal op.
//! 2. **Separator-aware compare** — a `/` in the suffix matches any single
//!    separator byte, `/` or `\` (GLOB_SPEC §12.3), so `**/main.ts` still
//!    matches `src\main.ts` on Windows.

use crate::engine::eq_byte;
use crate::engine::ops::Op;

/// The suffix facts extracted from one [`OpProgram`] (see module docs).
#[derive(Debug, Clone)]
pub struct LiteralFacts {
    /// The longest byte suffix every matching path must end with.
    pub suffix: Box<[u8]>,

    /// One suffix per branch, for a pattern ending with an
    /// `Op::Alternation` of literal branches that a single `suffix` can't
    /// cover. Empty means no set-based check (use `suffix` only).
    pub suffix_set: Box<[Box<[u8]>]>,

    /// ASCII case-insensitive compares, mirroring the program flag.
    pub case_insensitive: bool,
}

impl LiteralFacts {
    /// Extract facts from a linear op program.
    pub fn extract(ops: &[Op], case_insensitive: bool) -> Self {
        let suffix = extract_suffix(ops);
        let suffix_set = if suffix.is_empty() {
            extract_suffix_set(ops)
        } else {
            Vec::new()
        };
        Self {
            suffix: suffix.into_boxed_slice(),
            suffix_set: suffix_set
                .into_iter()
                .map(Vec::into_boxed_slice)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            case_insensitive,
        }
    }

    /// Cheap pre-filter: could `path` be a match? The case-sensitive path
    /// inlines into the caller; the CI path goes through a `#[cold]`
    /// dispatcher to keep it off the hot instruction cache.
    #[inline(always)]
    pub fn accept(&self, path: &[u8]) -> bool {
        if self.case_insensitive {
            return self.accept_ci_cold(path);
        }
        self.accept_inner::<false>(path)
    }

    #[inline]
    fn accept_inner<const CI: bool>(&self, path: &[u8]) -> bool {
        if !self.suffix.is_empty() {
            return ends_with_glob::<CI>(path, &self.suffix);
        }
        if !self.suffix_set.is_empty() {
            return self
                .suffix_set
                .iter()
                .any(|s| ends_with_glob::<CI>(path, s));
        }
        true
    }

    /// `#[cold]` CI dispatcher — keeps the case-insensitive body off the
    /// hot path.
    #[cold]
    fn accept_ci_cold(&self, path: &[u8]) -> bool {
        self.accept_inner::<true>(path)
    }
}

/// Walk ops right-to-left, prepending `Lit` / `Sep` bytes up to the first
/// non-literal op. The result is the guaranteed byte suffix of any match.
fn extract_suffix(ops: &[Op]) -> Vec<u8> {
    let mut acc: Vec<u8> = Vec::new();
    for op in ops.iter().rev() {
        match op {
            Op::Lit(bytes) => {
                let mut new_acc = Vec::with_capacity(bytes.len() + acc.len());
                new_acc.extend_from_slice(bytes);
                new_acc.extend_from_slice(&acc);
                acc = new_acc;
            }
            // Both strict `Sep` and lenient `SepRun` contribute a
            // single `/` to the suffix — the tail-anchored
            // `ends_with_glob` checker matches `/` against any one
            // separator byte, so the canonical single `/` is enough.
            Op::Sep | Op::SepRun => {
                let mut new_acc = Vec::with_capacity(1 + acc.len());
                new_acc.push(b'/');
                new_acc.extend_from_slice(&acc);
                acc = new_acc;
            }
            _ => break,
        }
    }
    acc
}

/// If the ops end with `Op::Alternation` where every branch is a pure
/// literal sequence, extract a suffix set: one suffix per branch,
/// each built by concatenating the branch's Lit ops with any trailing
/// Lit ops from the MAIN ops stream (before the Alternation).
///
/// For `**/*.{ts,tsx,js,jsx}` → ops `[OSS, Star, Lit("."), Alt([..])]`:
///   - common_tail (before Alt) contributes `"."`
///   - branches contribute `"ts"`, `"tsx"`, `"js"`, `"jsx"`
///   - result: `[".ts", ".tsx", ".js", ".jsx"]`
fn extract_suffix_set(ops: &[Op]) -> Vec<Vec<u8>> {
    // Find trailing Alternation.
    let alt_branches = match ops.last() {
        Some(Op::Alternation(branches)) => branches,
        _ => return Vec::new(),
    };

    // Extract the literal tail from ops BEFORE the Alternation.
    let pre_alt = &ops[..ops.len() - 1];
    let common_tail = extract_suffix(pre_alt);

    // Build a per-branch required path suffix.
    //
    // Two early-return checks (distinct, don't collapse):
    //   (A) non-literal branch whose trailing lit is empty
    //       (e.g. `{..Star}`): can't extract any reliable suffix, bail.
    //   (B) empty final suffix across the board: useless as a filter.
    //
    // Common_tail can only be safely prepended when the **entire branch**
    // is Lit/Sep — otherwise the branch has non-literal content (Star,
    // Class…) between common_tail and the branch's trailing literal,
    // and `common_tail + branch_suffix` is not a real path suffix.
    // Example: `test.{j*g,abc}` should yield `["g", "test.abc"]` (the
    // `*` in `j*g` breaks adjacency, so only `"g"` is reliable).
    let mut set = Vec::with_capacity(alt_branches.len());
    for branch in alt_branches {
        let branch_suffix = extract_suffix(branch);
        // (A)
        if branch_suffix.is_empty() && !branch.is_empty() {
            return Vec::new();
        }
        let branch_all_literal = branch
            .iter()
            .all(|op| matches!(op, Op::Lit(_) | Op::Sep | Op::SepRun));
        let full = if branch_all_literal {
            let mut v = Vec::with_capacity(common_tail.len() + branch_suffix.len());
            v.extend_from_slice(&common_tail);
            v.extend_from_slice(&branch_suffix);
            v
        } else {
            branch_suffix
        };
        // (B)
        if full.is_empty() {
            return Vec::new();
        }
        set.push(full);
    }
    set
}

/// Separator-aware `ends_with`.
///
/// A `/` in `suffix` matches any single separator byte in `path`
/// (i.e. `/` always, plus `\` on Windows). `CI=true` enables ASCII
/// case-insensitive byte equality. Strict — one `/` in the suffix
/// consumes exactly one separator byte from the path's tail.
#[inline]
fn ends_with_glob<const CI: bool>(path: &[u8], suffix: &[u8]) -> bool {
    let mut suffix_i = suffix.len();
    let mut path_i = path.len();
    while suffix_i > 0 {
        if path_i == 0 {
            return false;
        }
        suffix_i -= 1;
        path_i -= 1;
        let pb = suffix[suffix_i];
        let hb = path[path_i];
        if pb == b'/' {
            if !std::path::is_separator(hb as char) {
                return false;
            }
            continue;
        }
        if !eq_byte::<CI>(pb, hb) {
            return false;
        }
    }
    true
}
