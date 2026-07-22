//! SSM — segment-structured matcher. Primary engine for every pattern
//! whose ops decompose into a linear sequence of segment-shaped
//! elements (see `references/decisions/segment-engine-design.md`).
//!
//! A pattern compiles to one or more **element sequences** (more than
//! one only when a brace alternation crosses a `/`). Each element is
//! a literal segment, an in-segment wildcard matcher, or a globstar
//! absorber. Matching walks the path segment-at-a-time:
//!
//! - fixed-depth patterns: positional compare, one element per segment;
//! - single-globstar patterns (the dominant real-world shape): anchored
//!   head + anchored tail, the globstar absorbs the middle — no search;
//! - everything else: a tiny NFA over element positions, stepped once
//!   per *segment* (not per byte), active set in one `u64`.
//!
//! Compile is a linear scan over the lowered ops — no subset
//! construction, no hash maps. Patterns the segment model cannot
//! express (globstars glued mid-segment via braces, escaped
//! separators inside literals, fork/element/state counts over the
//! u64 budget) return the program back so the caller can fall through
//! to the Pike VM without re-lowering.
//!
//! Module map: [`compile`] turns ops into [`ElemSeq`]s, [`exec`] runs
//! them, [`seg_nfa`] is the mini NFA behind [`WildKind::Generic`].

mod compile;
mod exec;
mod seg_nfa;

use globstar::dir_match::DirMatch;
use globstar::engine::facts::LiteralFacts;
use globstar::engine::ops::{OpProgram, compute_static_prefixes};

use exec::{affix_eq, seq_match_dir, seq_matches};
use seg_nfa::SegNfa;

/// Fork budget: a pattern whose separator-crossing brace expansion
/// yields more element sequences than this falls back to the Pike VM.
/// Real patterns hold one or two forks; the cap only guards
/// adversarial `{a/b,c/d}{e/f,g/h}...` products.
const MAX_FORKS: usize = 64;

/// Element-NFA state budget per sequence (active set is one `u64`).
const MAX_SEQ_STATES: usize = 64;

#[inline(always)]
fn is_sep(b: u8) -> bool {
    std::path::is_separator(b as char)
}

// ---------------------------------------------------------------------------
// Compiled shape
// ---------------------------------------------------------------------------

/// One element of a sequence.
#[derive(Debug, Clone)]
enum Elem {
    /// Literal segment (possibly empty — `a//b` and trailing-`/`
    /// patterns produce empty literal segments). Never contains a
    /// separator byte.
    Lit(Box<[u8]>),
    /// In-segment wildcard matcher.
    Wild(Wild),
    /// Globstar absorbing ≥ 0 segments of any content (empty segments
    /// included). From `**/…`, mid `/**/`, and pattern-level `**`.
    G0,
    /// `G0` whose absorbed run may not *begin* with an empty segment.
    /// Arises when a brace fork splices a branch-internal `**/` behind
    /// a strict `Sep` (`a/{**/b,c}`), where the separator-run leniency
    /// of a native `/**/` boundary is absent.
    G0Strict,
    /// Globstar absorbing ≥ 1 segment. From trailing `/**`
    /// (`a/**` matches `a/` but not `a`) and from spliced bare `**`
    /// that sits behind or in front of a mandatory separator.
    G1,
}

impl Elem {
    #[inline]
    fn is_globstar(&self) -> bool {
        matches!(self, Elem::G0 | Elem::G0Strict | Elem::G1)
    }
}

/// In-segment matcher, classified at compile time. Dot protection is
/// baked in here (and inside [`SegNfa`]) — nothing about `dot` is
/// decided at match time for a `Wild`.
#[derive(Debug, Clone)]
struct Wild {
    kind: WildKind,
    /// Minimum segment byte length (literal parts + `?` count). For
    /// [`WildKind::AffixSet`] this excludes the per-branch suffix.
    /// Unused by [`WildKind::Generic`] (the NFA owns its bounds).
    min_len: u32,
    /// `false` ⇒ the length must equal the minimum exactly (no `*`).
    variable: bool,
    /// Reject dot-led segments (only ever set on `dot=false`
    /// compiles, and only when the matcher is wildcard-led — its
    /// first byte cannot come from a literal or a positive class).
    dot_protect: bool,
}

#[derive(Debug, Clone)]
enum WildKind {
    /// `lit1 (*|?)+ lit2` and degenerate forms: pure `*`/`?` runs
    /// (both affixes empty), `*lit`, `lit*`, `a?b` (no star — exact
    /// length via `variable=false`), plain multi-op literals.
    Affix { prefix: Box<[u8]>, suffix: Box<[u8]> },
    /// `lit (*|?)+ {lit,…}` — segment must carry the prefix and end
    /// with one of the suffixes (`*.{ts,tsx}`).
    AffixSet {
        prefix: Box<[u8]>,
        suffixes: Box<[Box<[u8]>]>,
    },
    /// Everything else (classes, non-tail alternations, interior
    /// literal islands): a mini Thompson NFA over the in-segment ops,
    /// simulated with a `u64` active set. Boxed so the rare slow-path
    /// variant doesn't size every [`Elem`] in the element table.
    Generic(Box<SegNfa>),
}

/// One compiled fork sequence plus its precomputed NFA metadata.
#[derive(Debug, Clone)]
struct ElemSeq {
    elems: Box<[Elem]>,
    /// Index of the single globstar element when the sequence has
    /// exactly one (fast anchored path); `usize::MAX` otherwise.
    single_g: usize,
    /// Number of globstar elements.
    g_count: usize,
    /// Single-globstar fast path: when every head element is a `Lit`,
    /// the joined head bytes (`"src/"` — each segment plus one `/`),
    /// so the head check is one sep-aware compare instead of a
    /// segment iteration. Empty ⇒ not applicable (no head, or a head
    /// element is non-literal).
    joined_head: Box<[u8]>,
    /// Element-NFA: per-element entry state id (ascending). Two-state
    /// elements (`G0Strict`, `G1`) own `entry` and `entry + 1`
    /// (body). The accept state is `num_states - 1`.
    state_of: Box<[u8]>,
    /// Inverse of `state_of`: owning element index per state (the
    /// accept state's slot is unused). Replaces a per-transition
    /// search in the element-NFA step.
    elem_of: Box<[u8]>,
    num_states: u8,
    /// `eps[s]` — states reachable from `s` via zero segment
    /// consumption (globstar "absorb nothing" skips), incl. `s`.
    eps: Box<[u64]>,
    /// States from which ≥ 1 further segment can be consumed on a
    /// path to accept. Drives `match_dir`'s prefix bit.
    reach1: u64,
    /// Per-fork quick reject: a byte suffix every match of THIS fork
    /// must end with (from a trailing `Lit` or `Affix` suffix; empty
    /// = no fact). Only consulted for multi-fork matchers, where the
    /// shared facts prefilter can't discriminate between forks.
    quick_suffix: Box<[u8]>,
}

/// Compiled segment-structured matcher.
#[derive(Debug, Clone)]
pub(crate) struct SegmentMatcher {
    seqs: Box<[ElemSeq]>,
    facts: LiteralFacts,
    prefixes: Box<[Box<[u8]>]>,
    case_insensitive: bool,
    dot: bool,
}

impl SegmentMatcher {
    /// Try to compile `program`. Returns the program back when the
    /// pattern is not segment-expressible so the caller can fall
    /// through to the Pike VM without re-lowering.
    pub(crate) fn build(program: OpProgram, dot: bool) -> Result<Box<Self>, OpProgram> {
        let ci = program.case_insensitive;
        let Some(seqs) = compile::compile_seqs(&program.ops, dot, ci) else {
            return Err(program);
        };
        // Eager on both runtimes: the walker prefixes are a cheap
        // leading-literal scan, and computing them here lets the
        // matcher drop every reference to the op tree.
        let prefixes = compute_static_prefixes(&program.ops);
        let facts = program.facts;
        Ok(Box::new(Self {
            seqs: seqs.into_boxed_slice(),
            facts,
            prefixes,
            case_insensitive: ci,
            dot,
        }))
    }

    /// Static path prefixes for walker traversal seeding.
    pub(crate) fn static_prefixes(&self) -> &[Box<[u8]>] {
        &self.prefixes
    }

    /// Whether `path` matches. The facts suffix prefilter answers the
    /// dominant reject case before any segment work.
    #[inline(always)]
    pub(crate) fn is_match(&self, path: &[u8]) -> bool {
        if !self.facts.accept(path) {
            return false;
        }
        if self.case_insensitive {
            self.is_match_slow::<true>(path)
        } else {
            self.is_match_slow::<false>(path)
        }
    }

    /// Post-prefilter body. `#[inline(never)]` keeps the (dominant)
    /// facts rejection path tiny inside `SegGlob::is_match`.
    #[inline(never)]
    fn is_match_slow<const CI: bool>(&self, path: &[u8]) -> bool {
        if self.seqs.len() == 1 {
            return seq_matches::<CI>(&self.seqs[0], path, self.dot);
        }
        // Multi-fork: each fork's own suffix fact rejects without the
        // tail scan-back (the shared facts prefilter only knows the
        // union of suffixes).
        self.seqs.iter().any(|seq| {
            let qs = &seq.quick_suffix;
            if !qs.is_empty() {
                let n = path.len();
                if n < qs.len() || !affix_eq::<CI>(qs, &path[n - qs.len()..]) {
                    return false;
                }
            }
            seq_matches::<CI>(seq, path, self.dot)
        })
    }

    /// Walker-style directory query — exact/prefix bits combined
    /// across forks.
    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        let (mut exact, mut prefix) = (false, false);
        for seq in self.seqs.iter() {
            let (e, p) = if self.case_insensitive {
                seq_match_dir::<true>(seq, dir_path, self.dot)
            } else {
                seq_match_dir::<false>(seq, dir_path, self.dot)
            };
            exact |= e;
            prefix |= p;
            if exact && prefix {
                break;
            }
        }
        DirMatch::from_exact_prefix(exact, prefix)
    }
}
