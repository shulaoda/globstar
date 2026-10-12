//! The segment engine: a pattern becomes a graph of per-segment
//! elements (literals, in-segment wildcard matchers, globstars) and a
//! path is matched segment by segment. A path through the graph with
//! at most one globstar is matched by anchored compares; everything
//! else runs an NFA whose active set grows with the pattern, so no
//! pattern is out of reach.

mod bits;
mod compile;
mod exec;
mod facts;
mod seg_nfa;

use crate::dir_match::DirMatch;
use crate::engine::ops::OpProgram;

use bits::{Bits, Wide};
use seg_nfa::SegNfa;

/// Paths through the graph `is_match` still tries one by one; past
/// that it runs the NFA over the whole graph, as `match_dir` always
/// does.
const MAX_CHAINS: usize = 64;
/// An `AffixSet` up to this size is scanned in pattern order; a
/// larger one is sorted and binary-searched per suffix length.
const LINEAR_SUFFIXES: usize = 8;

/// A byte string the matcher keeps: literals, affixes, suffixes,
/// static prefixes.
type Bytes = Box<[u8]>;

/// A 64-slot sketch of a set of bytes (`b % 64`): a byte not in the
/// sketch is not in the set, so a set scan can be skipped.
#[derive(Debug, Clone, Copy, Default)]
struct ByteSketch(u64);

impl ByteSketch {
    fn of_last_bytes<'a>(strings: impl Iterator<Item = &'a [u8]>, ci: bool) -> Self {
        let mut bits = 0u64;
        for s in strings {
            match s.last() {
                // The empty string ends with anything.
                None => return ByteSketch(u64::MAX),
                Some(&b) => {
                    bits |= 1 << (b & 63);
                    if ci {
                        bits |= 1 << (b.to_ascii_lowercase() & 63);
                        bits |= 1 << (b.to_ascii_uppercase() & 63);
                    }
                    // A trailing `/` stands for any separator.
                    if b == b'/' {
                        bits |= 1 << (b'\\' & 63);
                    }
                }
            }
        }
        ByteSketch(bits)
    }

    #[inline(always)]
    fn may_end(self, s: &[u8]) -> bool {
        match s.last() {
            None => self.0 == u64::MAX,
            Some(&b) => self.0 & (1 << (b & 63)) != 0,
        }
    }
}

#[derive(Debug, Clone)]
enum Elem {
    /// Literal segment (possibly empty; never contains a separator).
    Lit(Bytes),
    /// In-segment wildcard matcher.
    Wild(Wild),
    /// Globstar absorbing ≥ 0 segments, empty ones included.
    G0,
    /// `G0` whose absorbed run may not begin with an empty segment
    /// (a spliced `**/` behind a strict `Sep`, e.g. `a/{**/b,c}`).
    G0Strict,
    /// Globstar absorbing ≥ 1 segment (trailing `/**`: `a/**`
    /// matches `a/` but not `a`).
    G1,
}

impl Elem {
    #[inline]
    fn is_globstar(&self) -> bool {
        matches!(self, Elem::G0 | Elem::G0Strict | Elem::G1)
    }

    fn as_lit(&self) -> &[u8] {
        match self {
            Elem::Lit(lit) => lit,
            _ => unreachable!("not a literal"),
        }
    }

    /// Absorbs zero segments too.
    #[inline]
    fn nullable(&self) -> bool {
        matches!(self, Elem::G0 | Elem::G0Strict)
    }

    /// Takes every non-empty segment (`match_dir`'s all-below).
    fn any_seg(&self) -> bool {
        match self {
            Elem::Lit(_) => false,
            Elem::Wild(w) => w.any_seg,
            _ => true,
        }
    }

    /// NFA states the element owns: `G0Strict`/`G1` have a body state
    /// right after their entry.
    #[inline]
    fn states(&self) -> usize {
        if matches!(self, Elem::G0Strict | Elem::G1) {
            2
        } else {
            1
        }
    }

    /// Can the element consume some segment?
    fn satisfiable(&self) -> bool {
        match self {
            Elem::Wild(Wild {
                kind: WildKind::Generic(nfa),
                ..
            }) => nfa.satisfiable,
            _ => true,
        }
    }
}

#[derive(Debug, Clone)]
struct Wild {
    kind: WildKind,
    /// Minimum segment byte length (literal parts + `?` count; for
    /// suffix sets excludes the per-branch suffix, unused by
    /// `Generic`).
    min_len: u32,
    /// `false` ⇒ length must equal `min_len` exactly (no `*`).
    variable: bool,
    /// Reject dot-led segments (wildcard-led matcher on a
    /// `dot=false` compile).
    dot_protect: bool,
    /// Its wildcards alone match every non-empty segment (`*`, `?*`,
    /// `{a,*}`); drives `match_dir`'s all-below bit.
    any_seg: bool,
}

#[derive(Debug, Clone)]
enum WildKind {
    /// `lit (*|?)+ lit` and degenerate forms (`*`, `*lit`, `lit*`,
    /// `a?b`): compare the affixes, the length rules do the rest.
    Affix { prefix: Bytes, suffix: Bytes },
    /// `lit (*|?)+ {lit,…}` (`*.{ts,tsx}`): the prefix plus one
    /// suffix out of a set of at most `LINEAR_SUFFIXES`, scanned in
    /// pattern order.
    AffixSet {
        prefix: Bytes,
        suffixes: Box<[Bytes]>,
        last: ByteSketch,
    },
    /// `AffixSet` past `LINEAR_SUFFIXES` (a factored union of names).
    /// Boxed so the common variants keep their size.
    AffixIndex(Box<AffixIndex>),
    /// Everything else: a mini Thompson NFA over the in-segment ops.
    Generic(Box<SegNfa>),
}

/// A large suffix set: sorted by (length, bytes), ASCII-lowercased
/// when case-insensitive, with one `SuffixRun` per length for a
/// binary search.
#[derive(Debug, Clone)]
struct AffixIndex {
    prefix: Bytes,
    suffixes: Box<[Bytes]>,
    last: ByteSketch,
    /// One run per distinct suffix length, ascending.
    runs: Box<[SuffixRun]>,
}

#[derive(Debug, Clone, Copy)]
struct SuffixRun {
    len: u32,
    start: u32,
    end: u32,
}

/// `Chain::globstar` when the chain has none / several.
const NO_G: u32 = u32::MAX;
const MANY_G: u32 = u32::MAX - 1;

/// One path through the graph.
#[derive(Debug, Clone)]
struct Chain {
    /// Its nodes in order; empty for the lone path of a pattern
    /// without a separator-crossing brace, whose nodes are
    /// `0..elems.len()`.
    nodes: Box<[u32]>,
    /// Index of its globstar, or `NO_G` / `MANY_G`.
    globstar: u32,
    /// With several globstars: the NFA over this chain alone, unless
    /// the chain is the whole graph (then the matcher's own serves).
    nfa: Option<Box<Nfa<u64>>>,
}

/// An NFA over graph nodes: one state per node (two for `G0Strict`
/// and `G1`, the body state right after the entry) plus the accept
/// state, which is the last.
#[derive(Debug, Clone)]
struct Nfa<B: Bits> {
    states: Box<[State<B>]>,
    /// ε-closure of the start.
    init: B,
    /// States that can still consume ≥ 1 segment on a path to
    /// accept; drives `match_dir`'s prefix bit.
    reach1: B,
    /// Literal siblings looked up together (unions only).
    lit_sets: Option<Box<LitSets<B>>>,
}

#[derive(Debug, Clone)]
struct LitSets<B: Bits> {
    sets: Box<[LitSet<B>]>,
    /// Union of the sets' states; left out of the per-state step.
    states: B,
}

#[derive(Debug, Clone)]
struct State<B: Bits> {
    /// The closure entered after consuming a segment here (a
    /// globstar's includes itself).
    after: B,
    /// The state's node (the accept slot is unused).
    node: u32,
}

/// Literal nodes with the same predecessors: all active together, so
/// one search of the segment among their literals stands in for
/// testing each. The states, sorted by their node's literal
/// (case-folded when insensitive).
#[derive(Debug, Clone)]
struct LitSet<B: Bits> {
    states: B,
    sorted: Box<[u32]>,
}

#[derive(Debug, Clone)]
enum AnyNfa {
    Small(Nfa<u64>),
    Wide(Nfa<Wide>),
}

impl AnyNfa {
    fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        match self {
            AnyNfa::Small(n) => n.is_match(m, path),
            AnyNfa::Wide(n) => n.is_match(m, path),
        }
    }

    fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        match self {
            AnyNfa::Small(n) => n.match_dir(m, dir_path),
            AnyNfa::Wide(n) => n.match_dir(m, dir_path),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SegmentMatcher {
    /// The graph's nodes, in emission order.
    elems: Box<[Elem]>,
    /// The paths through the graph, tried in order; none past
    /// `MAX_CHAINS`.
    chains: Box<[Chain]>,
    /// The NFA over the graph, kept only when the chains cannot serve:
    /// none, several (`match_dir` runs them as one), or one with
    /// several globstars.
    nfa: Option<Box<AnyNfa>>,
    /// Prefilter: literal suffixes one of which every match ends
    /// with (empty: none known), sketched by last byte.
    suffixes: Box<[Bytes]>,
    suffix_last: ByteSketch,
    /// Literal path prefixes a walker can seed traversal from.
    prefixes: Box<[Bytes]>,
    case_insensitive: bool,
    dot: bool,
}

impl SegmentMatcher {
    pub(crate) fn build(program: OpProgram, dot: bool) -> Box<Self> {
        Box::new(compile::compile(
            &program.ops,
            dot,
            program.case_insensitive,
        ))
    }

    /// Every matching path starts with one of these.
    pub(crate) fn static_prefixes(&self) -> Vec<Vec<u8>> {
        self.prefixes.iter().map(|p| p.to_vec()).collect()
    }

    #[inline]
    pub(crate) fn is_match(&self, path: &[u8]) -> bool {
        if !self.prefilter(path) {
            return false;
        }
        match (&self.chains[..], &self.nfa) {
            ([], Some(nfa)) => nfa.is_match(self, path),
            ([_], _) => self.chain_matches(0, path),
            (chains, _) => {
                (0..chains.len()).any(|i| self.ends_like(i, path) && self.chain_matches(i, path))
            }
        }
    }

    /// `is_match` through an NFA only (test hook): the matcher's own,
    /// or one laid out over the lone chain it kept instead.
    pub(crate) fn is_match_nfa(&self, path: &[u8]) -> bool {
        if !self.prefilter(path) {
            return false;
        }
        match &self.nfa {
            Some(nfa) => nfa.is_match(self, path),
            None => compile::chain_nfa(self).is_match(self, path),
        }
    }

    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        match &self.nfa {
            Some(nfa) => nfa.match_dir(self, dir_path),
            None => self.chain_dir(dir_path),
        }
    }
}
