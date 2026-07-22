//! Linear instruction stream lowered from the AST.
//!
//! Every matcher (Tier 0 literal, Thompson DFA, Pike VM) consumes an
//! [`OpProgram`] rather than the AST directly.
//! This separation lets us:
//!
//! 1. Run optimization passes on the linear form (literal merging, dead-op
//!    elimination, globstar folding)
//! 2. Share AST → IR lowering between multiple matchers
//! 3. Make the matcher loop tight and cache-friendly
//!
//! ## Globstar folding (spec D-003 asymmetric semantics)
//!
//! Raw lowering emits `Op::Globstar` adjacent to `Op::Sep`. A post-pass folds
//! these into dedicated ops:
//!
//! | Source         | Folded op          | Regex analogue    |
//! |----------------|--------------------|-------------------|
//! | `**/`  (lead)  | `OptSegmentsSlash` | `(?:[^/]*/)*`     |
//! | `/**/` (mid)   | `OptSegmentsSlash` (with preceding `Sep` kept) | same |
//! | `/**`  (trail) | `SlashAnything`    | `/.*`             |
//! | `**`   (alone) | `GlobstarAny`      | `.*`              |
//!
//! **Asymmetric semantics (D-003):**
//! - `**/foo` matches `foo` (leading `**/` collapses to empty)
//! - `a/**/b` matches `a/b` (middle `/**/` collapses to just `/`)
//! - `a/**` does **NOT** match `a` — the trailing `/**` requires a real `/`
//!   in the path. `L(a/**) = a/.*` i.e. "anything strictly under `a/`".
//!
//! Picomatch treats `a/**` as optional (`a(?:/.*)?`, matches `a` too). We
//! deliberately diverge because the "strictly under" interpretation matches
//! walker expectations — listing contents of `a/` should not include `a`
//! itself.
//!
//! ## Brace alternation
//!
//! `{a,b,c}` is lowered as [`Op::Alternation`] — each branch becomes a
//! sub-sequence of ops. The matcher tries each branch at the alternation
//! point and succeeds if any matches. Nested braces produce nested
//! `Op::Alternation` nodes. No cartesian expansion is performed — memory
//! and match cost are independent of the number of brace variants.

use crate::ast::{CharClass, Node};
use crate::engine::facts::LiteralFacts;

/// One executable instruction in a compiled glob program.
#[derive(Debug, Clone)]
pub enum Op {
    /// Match a literal byte sequence verbatim.
    Lit(Vec<u8>),
    /// Match a single non-separator byte.
    AnyChar,
    /// Match zero or more non-separator bytes (Kleene star, single segment).
    Star,
    /// Match one byte against the class.
    Class(CharClass),
    /// Match a single path separator (any byte in the `Seps` set —
    /// §12.3). Strict — consumes exactly one separator byte. Aligned
    /// with `picomatch` / `globset` / `bash` on simple `/` patterns:
    /// `a/b` does not match `a//b`.
    Sep,
    /// Match a run of one or more path separators. Used at `**`
    /// boundaries (mid `/**/`) where `picomatch` and friends are
    /// lenient on separator runs to absorb path-side `//` produced
    /// by ordinary path concatenation. Inserted by the globstar fold
    /// pass; never emitted by `lower_into` directly.
    SepRun,
    /// Raw `**`. Emitted by `lower_into` before globstar folding; should
    /// never reach the matcher.
    Globstar,
    /// Matches `(?:[^/]*/)*` — zero or more `<segment>/` repetitions.
    /// The picomatch lowering of `**/` (at start of pattern or when the
    /// preceding Sep is kept for `/**/`). Can match empty.
    OptSegmentsSlash,
    /// Matches `/.*` — a required `/` followed by any number of chars
    /// (with dot protection at each new segment). The lowering of `/**`
    /// at the end of the pattern; the preceding `/` is absorbed and the
    /// globstar becomes "anything strictly under this slash". See D-003
    /// for why this is NOT optional (contrast with `OptSegmentsSlash`).
    SlashAnything,
    /// Matches `.*` — anything, including empty. The lowering of bare `**`
    /// (when it's the entire pattern, or preceded only by another globstar).
    GlobstarAny,
    /// Matches zero or more leading path separators — `/` always, plus
    /// `\` on platforms where `std::path::is_separator('\\')` is true
    /// (i.e. Windows). Nullable, so relative paths still match.
    ///
    /// Prepended during lowering when a pattern starts with `**/` (its
    /// first op after fold is `OptSegmentsSlash`), so that `**/X`
    /// uniformly means "X at any depth" — covering relative paths,
    /// Unix absolute paths (`/a/X`), and UNC paths (`//server/X`).
    /// See GLOB_SPEC §8.4.
    LeadingSeps,
    /// Brace alternation: match exactly one of the branches. Each inner
    /// `Vec<Op>` is one branch's flat op sequence. Branches CAN cross
    /// path separators (e.g. `{src/lib,tests}`).
    ///
    /// Produced by `lower_into` when encountering `Node::Brace`. Each
    /// branch has already been through `fold_globstars_inplace` and
    /// literal merging.
    Alternation(Vec<Vec<Op>>),
}

#[derive(Debug, Clone)]
pub struct OpProgram {
    pub ops: Vec<Op>,
    /// Literal prefix/suffix facts for fast `is_match` pre-filtering.
    /// Computed once at construction time and reused on every match call.
    pub facts: LiteralFacts,
    /// ASCII case-insensitive matching flag — propagated to all matchers
    /// so literal byte compares can use `eq_ignore_ascii_case`. Classes
    /// are pre-expanded at lowering time and do not need to consult
    /// this flag at match time.
    pub case_insensitive: bool,
}

impl OpProgram {
    /// Construct a program from the lowered op stream, eagerly extracting
    /// the [`LiteralFacts`] pre-filter.
    pub fn new(ops: Vec<Op>, case_insensitive: bool) -> Self {
        let facts = LiteralFacts::extract(&ops, case_insensitive);
        Self {
            ops,
            facts,
            case_insensitive,
        }
    }

    /// Number of ops in the program.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether the program contains no ops (matches the empty input only).
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Lower an AST node into a single [`OpProgram`].
///
/// Brace nodes are preserved as [`Op::Alternation`] in the ops stream
/// rather than cartesian-expanded into separate programs. This means
/// one NFA handles all brace variants, keeping memory and match cost
/// independent of variant count.
///
/// When `case_insensitive` is set, [`Op::Class`] items are expanded at
/// this stage to include ASCII case-alternates; [`Op::Lit`] bytes stay
/// verbatim and the flag is stored on the returned [`OpProgram`] for
/// runtime compare helpers to consult.
pub fn lower(node: &Node, case_insensitive: bool) -> OpProgram {
    let mut ops = Vec::new();
    // §7 expansion equation: a separator adjacent to a brace whose
    // branch edge holds a globstar belongs to EVERY expansion, so it
    // is distributed into the branches before lowering — this is what
    // lets `{**,x}/b` mean `**/b ∪ x/b` (matching `b`) and gives
    // `a/{**/x,y}` the same lenient `/**/` boundary as `a/**/x`.
    // Cheap scan first: virtually all patterns skip the clone.
    if needs_sep_distribution(node) {
        let distributed = distribute_seps(node.clone());
        lower_into(&distributed, &mut ops, case_insensitive);
    } else {
        lower_into(node, &mut ops, case_insensitive);
    }
    fold_globstars_inplace(&mut ops);
    // Pattern-start `**/` semantics (GLOB_SPEC §8.4): `**/X` means "X at
    // any depth", covering relative, Unix-absolute, and UNC paths.
    // Prepending `LeadingSeps` lets the matcher naturally consume any
    // leading separator run without affecting middle-of-pattern `**/`.
    //
    // For top-level brace/alternation at pattern start, recurse per
    // branch: `{**/a,README}` must prepend LeadingSeps to only the
    // `**/a` branch so that `/README` doesn't spuriously match the
    // plain-literal branch.
    apply_leading_seps_at_start(&mut ops);
    OpProgram::new(ops, case_insensitive)
}

/// Prepend `Op::LeadingSeps` to any `**/`-rooted branch at pattern
/// start. Recurses into [`Op::Alternation`] so nested braces are
/// handled (e.g. `{{**/a,x},y}` applies LeadingSeps only to the
/// `**/a` path, not to `x` or `y`).
fn apply_leading_seps_at_start(ops: &mut Vec<Op>) {
    match ops.first_mut() {
        Some(Op::OptSegmentsSlash) => {
            ops.insert(0, Op::LeadingSeps);
        }
        Some(Op::Alternation(branches)) => {
            for branch in branches.iter_mut() {
                apply_leading_seps_at_start(branch);
            }
        }
        _ => {}
    }
}

/// Extract the longest segment-bounded literal prefix from an op program.
///
/// The returned bytes always satisfy one of:
/// - The program is fully literal (Lit/Sep only, no wildcards/etc.)
///   and the result is the entire concatenated literal content.
/// - The program has at least one wildcard-like op; the result is the
///   literal content up to and ending at the most recent segment boundary
///   (i.e., immediately after a `Sep`), with any trailing `/` stripped so
///   the result is suitable as a `relative` path for the walker.
///
/// Examples:
/// - `src/main.rs` (Tier 0 literal, not Backtrack) is handled at the caller
/// - `src/*.ts` → `b"src"`
/// - `src/main/*.ts` → `b"src/main"`
/// - `src/mai*.ts` → `b"src"` (stops at the last segment boundary)
/// - `**/*.ts` → `b""` (first op is a wildcard absorber)
pub fn extract_prefix(ops: &[Op]) -> Vec<u8> {
    let mut acc: Vec<u8> = Vec::new();
    let mut last_boundary: usize = 0;
    let mut fully_literal = true;
    for op in ops {
        match op {
            Op::Lit(bytes) => acc.extend_from_slice(bytes),
            // `Sep` and `SepRun` both contribute a `/` to the static
            // prefix. `SepRun` represents 1+ separator bytes (mid-`**`
            // boundary); for walker prefix-jump purposes the canonical
            // single `/` is enough, and the trailing-`/` strip below
            // normalizes the result.
            Op::Sep | Op::SepRun => {
                acc.push(b'/');
                last_boundary = acc.len();
            }
            _ => {
                fully_literal = false;
                break;
            }
        }
    }
    if !fully_literal {
        acc.truncate(last_boundary);
    }
    // Strip trailing `/` so the result is a valid relative-path segment
    // (the walker always adds its own separator between parent and child).
    while acc.last() == Some(&b'/') {
        acc.pop();
    }
    acc
}

/// Compute the deduplicated set of static path prefixes for a
/// program — the walker uses this to seed traversal at the deepest
/// pre-determined directory.
///
/// `Op::Alternation` at the start of the program (e.g. brace
/// expansion) yields one prefix per branch; everything else returns a
/// single `extract_prefix` result. Output is deduped via
/// [`dedupe_prefixes`] so `[src, src/cli]` collapses to `[src]`.
///
/// Cached by [`super::thompson_dfa::ThompsonDfa`] and
/// [`super::pikevm::PikeVm`] at build time so the (potentially heavy)
/// `OpProgram` tree can be dropped — the program's own size dominates
/// memory for wide brace patterns (e.g. 64-way `{a,b,…}`), while the
/// cached prefix set is at most a few hundred bytes.
pub fn compute_static_prefixes(ops: &[Op]) -> Box<[Box<[u8]>]> {
    let raw = extract_prefixes_per_branch(ops);
    let deduped = dedupe_prefixes(raw);
    deduped
        .into_iter()
        .map(|v| v.into_boxed_slice())
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Per-branch prefix extraction. Brace alternation at the head
/// produces one prefix per branch; otherwise a single prefix.
fn extract_prefixes_per_branch(ops: &[Op]) -> Vec<Vec<u8>> {
    match ops.first() {
        Some(Op::Alternation(branches)) => branches
            .iter()
            .flat_map(|branch| extract_prefixes_per_branch(branch))
            .collect(),
        _ => vec![extract_prefix(ops)],
    }
}

/// Deduplicate a list of static prefixes, keeping only minimal
/// (maximal-coverage) entries. If `a` is a segment-boundary prefix of
/// `b`, `b` is dropped since walking `a` already visits `b`'s subtree.
/// The empty prefix subsumes everything: `[b"", b"src"]` → `[b""]`.
pub(crate) fn dedupe_prefixes(mut prefixes: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    // Shorter candidates first — the single pass below handles exact
    // duplicates too, since `is_dir_prefix_of(p, p)` is true for any
    // `p`, so the second occurrence is filtered as "prefix of itself".
    prefixes.sort_by_key(|p| p.len());
    let mut result: Vec<Vec<u8>> = Vec::with_capacity(prefixes.len());
    for p in prefixes {
        if !result.iter().any(|r| is_dir_prefix_of(r, &p)) {
            result.push(p);
        }
    }
    result
}

/// Whether `short` is a directory-boundary prefix of `long`. Empty
/// `short` is always a prefix. Otherwise `long` must start with
/// `short` AND the next byte (if any) must be `/`.
fn is_dir_prefix_of(short: &[u8], long: &[u8]) -> bool {
    if short.is_empty() {
        return true;
    }
    if long.len() < short.len() || !long.starts_with(short) {
        return false;
    }
    long.len() == short.len() || long[short.len()] == b'/'
}

/// Push an op into `out`, merging adjacent `Op::Lit` inline. Called from
/// [`lower_into`] so a brace-expanded `Lit(".") + Lit("ts")` run collapses
/// to `Lit(".ts")` without a separate merge pass.
fn push_op(out: &mut Vec<Op>, op: Op) {
    if let Op::Lit(bytes) = op {
        if let Some(Op::Lit(prev)) = out.last_mut() {
            prev.extend_from_slice(&bytes);
            return;
        }
        out.push(Op::Lit(bytes));
        return;
    }
    out.push(op);
}

/// Does any brace in the tree sit next to a separator while holding a
/// branch-edge globstar? (Trigger for [`distribute_seps`].)
fn needs_sep_distribution(node: &Node) -> bool {
    match node {
        Node::Concat(children) => {
            for (i, child) in children.iter().enumerate() {
                if let Node::Brace(branches) = child {
                    let prev_sep = i > 0 && matches!(children[i - 1], Node::Separator);
                    let next_sep = matches!(children.get(i + 1), Some(Node::Separator));
                    if (prev_sep && branches.iter().any(leads_globstar))
                        || (next_sep && branches.iter().any(trails_globstar))
                    {
                        return true;
                    }
                }
                if needs_sep_distribution(child) {
                    return true;
                }
            }
            false
        }
        Node::Brace(branches) => branches.iter().any(needs_sep_distribution),
        _ => false,
    }
}

fn leads_globstar(node: &Node) -> bool {
    match node {
        Node::Globstar => true,
        Node::Concat(xs) => xs.first().is_some_and(leads_globstar),
        Node::Brace(bs) => bs.iter().any(leads_globstar),
        _ => false,
    }
}

fn trails_globstar(node: &Node) -> bool {
    match node {
        Node::Globstar => true,
        Node::Concat(xs) => xs.last().is_some_and(trails_globstar),
        Node::Brace(bs) => bs.iter().any(trails_globstar),
        _ => false,
    }
}

/// Push separators that flank globstar-edged braces into every branch
/// (recursively — an absorbed separator next to a nested brace keeps
/// sinking). A separator shared by two qualifying braces goes to the
/// LEFT brace's tails (deterministic; the pathological
/// `{a,**}/{**,b}` shape keeps the splice semantics for its right
/// side).
fn distribute_seps(node: Node) -> Node {
    match node {
        Node::Concat(children) => {
            let mut out: Vec<Node> = Vec::with_capacity(children.len());
            let mut iter = children.into_iter().peekable();
            while let Some(child) = iter.next() {
                let Node::Brace(branches) = child else {
                    out.push(distribute_seps(child));
                    continue;
                };
                let absorb_prev = matches!(out.last(), Some(Node::Separator))
                    && branches.iter().any(leads_globstar);
                let absorb_next = matches!(iter.peek(), Some(Node::Separator))
                    && branches.iter().any(trails_globstar);
                if !absorb_prev && !absorb_next {
                    out.push(distribute_seps(Node::Brace(branches)));
                    continue;
                }
                if absorb_prev {
                    out.pop();
                }
                if absorb_next {
                    iter.next();
                }
                let branches = branches
                    .into_iter()
                    .map(|b| {
                        let mut seq = Vec::with_capacity(3);
                        if absorb_prev {
                            seq.push(Node::Separator);
                        }
                        seq.push(b);
                        if absorb_next {
                            seq.push(Node::Separator);
                        }
                        // Re-distribute: the absorbed separator may
                        // now flank a nested globstar-edged brace.
                        distribute_seps(Node::Concat(seq))
                    })
                    .collect();
                out.push(Node::Brace(branches));
            }
            Node::Concat(out)
        }
        Node::Brace(branches) => {
            Node::Brace(branches.into_iter().map(distribute_seps).collect())
        }
        other => other,
    }
}

fn lower_into(node: &Node, out: &mut Vec<Op>, case_insensitive: bool) {
    match node {
        Node::Literal(bytes) => push_op(out, Op::Lit(bytes.clone())),
        Node::Separator => push_op(out, Op::Sep),
        Node::AnyChar => push_op(out, Op::AnyChar),
        Node::Star => push_op(out, Op::Star),
        Node::Globstar => push_op(out, Op::Globstar),
        Node::Class(c) => {
            let cls = if case_insensitive {
                c.expanded_ascii_case_insensitive()
            } else {
                c.clone()
            };
            push_op(out, Op::Class(cls));
        }
        Node::Concat(children) => {
            for child in children {
                lower_into(child, out, case_insensitive);
            }
        }
        Node::Brace(branches) => {
            let mut alt_branches = Vec::with_capacity(branches.len());
            for branch in branches {
                let mut branch_ops = Vec::new();
                lower_into(branch, &mut branch_ops, case_insensitive);
                fold_globstars_inplace(&mut branch_ops);
                alt_branches.push(branch_ops);
            }
            push_op(out, Op::Alternation(alt_branches));
        }
    }
}

/// In-place fold of raw `Op::Globstar` + adjacent `Op::Sep` into the
/// dedicated picomatch-style globstar ops. Uses a two-pointer write-index
/// pattern over the supplied `Vec<Op>` — no intermediate allocations.
///
/// Sub-passes (all in place):
/// 1. `[Globstar, Sep]` → `OptSegmentsSlash` — lead and middle `**/`.
///    The middle `/**/` works because the preceding `Sep` is preserved
///    and OSS's `(?:[^/]*/)*` body can match empty.
///
///    1b. `[Sep, OptSegmentsSlash]` → `[SepRun, OSS]` — upgrade the
///    explicit separator before mid-`**/` to a lenient run absorber,
///    so paths like `a//b` still match `a/**/b` (matches `picomatch`
///    / `globset` / `wax`). The plain `Sep` is strict; only the
///    boundary adjacent to `**` is lenient.
/// 2. `[Sep, Globstar]` → `SlashAnything` — trailing `/**`. Non-optional
///    per D-003: `a/**` requires a real `/` and does NOT match `a`.
/// 3. Any remaining bare `Op::Globstar` → `Op::GlobstarAny`.
fn fold_globstars_inplace(ops: &mut Vec<Op>) {
    // Pass 1 — forward scan, Globstar + Sep → OSS.
    let mut write = 0usize;
    let mut read = 0usize;
    while read < ops.len() {
        if matches!(ops[read], Op::Globstar) && matches!(ops.get(read + 1), Some(Op::Sep)) {
            ops[write] = Op::OptSegmentsSlash;
            read += 2;
            write += 1;
            continue;
        }
        if read != write {
            ops.swap(write, read);
        }
        read += 1;
        write += 1;
    }
    ops.truncate(write);

    // Pass 1b — upgrade `Sep` immediately before `OSS` to `SepRun`
    // (lenient 1+). This is the boundary that came in as `/**/` mid:
    // strict on the standalone `Sep` would make `a/**/b` reject
    // `a//b`, diverging from picomatch / globset / wax. Run after
    // Pass 1 so all OSS absorbers are in place.
    for i in 0..ops.len().saturating_sub(1) {
        if matches!(ops[i], Op::Sep) && matches!(ops[i + 1], Op::OptSegmentsSlash) {
            ops[i] = Op::SepRun;
        }
    }

    // Pass 2 — forward scan, Sep + Globstar → SlashAnything. Runs on the
    // already-compressed ops so pass 1's OSS absorbers are respected.
    let mut write = 0usize;
    let mut read = 0usize;
    while read < ops.len() {
        if matches!(ops[read], Op::Sep) && matches!(ops.get(read + 1), Some(Op::Globstar)) {
            ops[write] = Op::SlashAnything;
            read += 2;
            write += 1;
            continue;
        }
        if read != write {
            ops.swap(write, read);
        }
        read += 1;
        write += 1;
    }
    ops.truncate(write);

    // Pass 3 — rewrite any remaining bare `Globstar` (standalone `**` or
    // `**` next to another absorbed globstar) as `GlobstarAny`.
    for op in ops.iter_mut() {
        if matches!(op, Op::Globstar) {
            *op = Op::GlobstarAny;
        }
    }
}
