//! Suffix-anchored pre-filter run before every engine's `is_match`.
//!
//! Every [`OpProgram`](super::ops::OpProgram) carries a [`LiteralFacts`]
//! recording the byte suffix (or a set of them, for a trailing brace
//! alternation) that every matching path must end with. The matcher checks
//! it with a separator-aware `ends_with` before running the engine:
//!
//! ```text
//! path ends with suffix  →  maybe a match, run the engine
//!         otherwise       →  definitely not
//! ```
//!
//! On walker workloads this rejects most candidates outright. `src/**/*.ts`
//! drops every `.js` file in one suffix scan. Only the suffix is recorded,
//! not a prefix, because the engines already scan left to right and the tail
//! is the one anchor they can't check up front.
//!
//! ## Correctness invariant
//!
//! `accept(path) == false` must mean no program variant can match `path`, so
//! the filter must never reject a path the engine would accept. Two rules
//! keep it safe.
//!
//! 1. Conservative extraction. Stop at the first non-literal op.
//! 2. Separator-aware compare. A `/` in the suffix matches any one separator
//!    byte, `/` or `\` (GLOB_SPEC §12.3), so `**/main.ts` still matches
//!    `src\main.ts` on Windows.

use crate::engine::eq_byte;
use crate::engine::ops::Op;

/// The suffix facts extracted from one [`OpProgram`] (see module docs).
#[derive(Debug, Clone)]
pub struct LiteralFacts {
    /// The longest byte suffix every matching path must end with.
    pub suffix: Box<[u8]>,

    /// One suffix per branch, for a pattern ending in an `Op::Alternation` of
    /// literal branches that a single `suffix` can't cover. Empty means no
    /// set check, use `suffix` alone.
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

    /// Cheap pre-filter: could `path` be a match?
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

    #[cold]
    fn accept_ci_cold(&self, path: &[u8]) -> bool {
        self.accept_inner::<true>(path)
    }
}

/// Walk ops right-to-left, collecting `Lit` and `Sep` bytes until the first
/// non-literal op. The result is the byte suffix every match must end with.
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
            // Sep and SepRun both contribute one `/`. `ends_with_glob` matches
            // it against any single separator byte, so one is enough.
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

/// When the ops end in an `Op::Alternation` of pure-literal branches, build
/// one suffix per branch by gluing the branch's literals to any literal tail
/// before the alternation.
///
/// `**/*.{ts,tsx,js,jsx}` lowers to `[OSS, Star, Lit("."), Alt([..])]`, whose
/// common tail `"."` glues to each branch to give `[".ts", ".tsx", ".js",
/// ".jsx"]`.
fn extract_suffix_set(ops: &[Op]) -> Vec<Vec<u8>> {
    let alt_branches = match ops.last() {
        Some(Op::Alternation(branches)) => branches,
        _ => return Vec::new(),
    };

    let pre_alt = &ops[..ops.len() - 1];
    let common_tail = extract_suffix(pre_alt);

    // common_tail is only safe to prepend when the whole branch is Lit/Sep.
    // Otherwise non-literal content (Star, Class) sits between it and the
    // branch's trailing literal, so `common_tail + branch_suffix` is not a
    // real suffix. `test.{j*g,abc}` yields `["g", "test.abc"]`, since the `*`
    // breaks adjacency and only `"g"` stays reliable.
    //
    // (A) and (B) bail when a branch yields no reliable suffix.
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

/// Separator-aware `ends_with`. A `/` in `suffix` matches any single separator
/// byte (`/` always, `\` on Windows). `CI=true` compares ASCII
/// case-insensitively.
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
