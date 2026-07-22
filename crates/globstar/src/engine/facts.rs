//! Compile-time literal facts for fast pre-filtering in `is_match`.
//!
//! Every [`OpProgram`](super::ops::OpProgram) carries a [`LiteralFacts`]
//! that records the byte **suffix** (or set of suffixes for trailing
//! brace alternations) every matching path must end with. Before
//! invoking the engine, the matcher short-circuits on a cheap `accept`
//! check:
//!
//! ```text
//! path.ends_with(suffix)  →  maybe match
//!         otherwise        →  definitely not
//! ```
//!
//! This is the filter-then-verify idea from ADR-004: a constant-time
//! rejection layer in front of the matcher. Typical walker scenarios
//! see 90%+ of candidate paths rejected by the suffix check alone
//! (e.g. `src/**/*.ts` rejecting every `.js` file without running the
//! exact engine).
//!
//! Prefix matching used to live here too, but it forced two scans of
//! the same bytes. It moved into each engine's natural left-to-right
//! execution path. Suffix is uniquely valuable because the engine is
//! left-to-right and can't cheaply check a tail anchor without
//! running the full match.
//!
//! ## Correctness invariant
//!
//! `accept(path)` returns `false` → no program variant can match `path`.
//! `accept(path)` returns `true` → match is possible; run the real matcher.
//!
//! The filter must therefore **never** reject a path that the matcher
//! would accept. This drives two design choices:
//!
//! 1. **Conservative extraction**: at a brace / wildcard boundary,
//!    suffix extraction stops. We do not try to factor common literals
//!    out of sibling branches (a future optimization).
//! 2. **Separator-aware byte matching**: `/` in the suffix matches any
//!    run of `/` or `\` in the path (GLOB_SPEC §12.3). A strict byte
//!    `ends_with` would incorrectly reject `src\main.ts` for pattern
//!    `**/main.ts` on Windows.

use crate::engine::eq_byte;
use crate::engine::ops::Op;

/// Literal facts extracted from one [`OpProgram`]. See module docs.
///
/// Only **suffix** facts are stored — prefix matching is left to the
/// engines, which naturally walk the prefix bytes and reject
/// at the same speed as a separate byte-compare would. Carrying a
/// prefix in facts forced two scans of the same bytes. The
/// suffix is uniquely valuable here because the engines are
/// left-to-right — they can't cheaply check a tail anchor without
/// running the full match, so a separate ends-with check pre-rejects
/// huge swathes of mismatched paths (`**/*.md` against a `.ts` file
/// rejects in O(suffix.len()) without ever entering the engine).
#[derive(Debug, Clone, Default)]
pub struct LiteralFacts {
    /// The longest byte suffix every matching path must end with.
    /// `Box<[u8]>` rather than `Vec<u8>` — saves 8 B inline (no `cap`
    /// field) and signals the post-construction immutability.
    pub suffix: Box<[u8]>,
    /// For patterns ending with `Op::Alternation` of literal branches:
    /// the path must end with at least one of these suffixes. Populated
    /// by `extract_suffix_set` when a single `suffix` can't cover all
    /// branches. Empty means no set-based check (use `suffix` only).
    /// Same `Box<[…]>` rationale as [`Self::suffix`] — the outer slice
    /// and each inner suffix are both immutable post-build.
    pub suffix_set: Box<[Box<[u8]>]>,
    /// ASCII case-insensitive compare flag — mirrors the program flag so
    /// `accept` can compare with case folding when the matcher would.
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

    /// Cheap pre-filter: is `path` possibly a match?
    ///
    /// Strict path is the function fall-through and inlines into the
    /// calling matcher; CI path goes via a `#[cold]` dispatcher so LLVM
    /// keeps the CI body out of the hot icache region (same pattern as
    /// [`LiteralMatcher::is_match`](crate::engine::literal::LiteralMatcher)).
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

    /// Cold dispatcher for the CI-side [`Self::accept_inner`] invocation.
    /// Same `#[cold]` rationale as
    /// [`LiteralMatcher::is_match`](crate::engine::literal::LiteralMatcher).
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
