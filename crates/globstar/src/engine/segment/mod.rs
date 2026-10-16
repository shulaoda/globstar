//! The segment engine: a pattern becomes a graph of per-segment
//! elements (literals, in-segment wildcard matchers, globstars) and a
//! path is matched segment by segment. A path through the graph with
//! at most one globstar is matched by anchored compares; everything
//! else runs an NFA whose active set grows with the pattern, so no
//! pattern is too big for it. Only braces whose segments multiply are
//! refused, by the compile walk (`GlobError::TooComplex`).

mod bits;
mod compile;
mod exec;
mod facts;
mod seg_nfa;

use crate::dir_match::DirMatch;
use crate::engine::ops::OpProgram;
use crate::error::GlobError;

use bits::{Bits, Wide};
use seg_nfa::SegNfa;

/// Paths through the graph `is_match` tries one by one; past that the
/// NFA over the whole graph runs.
const MAX_CHAINS: usize = 64;
/// An `AffixSet` up to this size is scanned in pattern order; a
/// larger one is sorted and binary-searched per suffix length.
const LINEAR_SUFFIXES: usize = 8;

/// A byte string the matcher keeps: literals, affixes, suffixes,
/// static prefixes.
type Bytes = Box<[u8]>;

/// The `min_len` of a segment no path segment can fill: one with an
/// escaped `\` on Windows, where a segment never holds a separator.
const NEVER: u32 = u32::MAX;

/// Does `lit` hold a separator byte (an escaped `\` on Windows)? A
/// literal never holds a `/`, so elsewhere it never does.
fn has_sep(lit: &[u8]) -> bool {
    cfg!(windows) && lit.iter().any(|&b| std::path::is_separator(char::from(b)))
}

/// A 64-slot sketch of a set of bytes (`b % 64`): a byte not in the
/// sketch is not in the set, so a set scan can be skipped.
#[derive(Debug, Clone, Copy)]
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

    #[inline]
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
    /// Globstar absorbing ≥ 1 segment (trailing `/**`: `a/**`
    /// matches `a/` but not `a`).
    G1,
}

impl Elem {
    #[inline]
    fn is_globstar(&self) -> bool {
        matches!(self, Elem::G0 | Elem::G1)
    }

    #[inline]
    fn as_lit(&self) -> &[u8] {
        match self {
            Elem::Lit(lit) => lit,
            _ => unreachable!("not a literal"),
        }
    }

    /// Absorbs zero segments too.
    #[inline]
    fn nullable(&self) -> bool {
        matches!(self, Elem::G0)
    }

    /// Takes every non-empty segment (`match_dir`'s all-below).
    #[inline]
    fn any_seg(&self) -> bool {
        match self {
            Elem::Lit(_) => false,
            Elem::Wild(w) => w.any_seg,
            _ => true,
        }
    }

    /// Can the element consume some segment?
    #[inline]
    fn satisfiable(&self) -> bool {
        match self {
            Elem::Wild(w) if w.min_len == NEVER => false,
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

/// The rarer kinds are boxed so an `Elem` stays 40 bytes.
#[derive(Debug, Clone)]
enum WildKind {
    /// `lit (*|?)+ lit` and degenerate forms (`*`, `*lit`, `lit*`,
    /// `a?b`): compare the affixes, the length rules do the rest.
    Affix(Affix),
    /// `lit (*|?)+ {lit,…}` (`*.{ts,tsx}`): the prefix plus one
    /// suffix out of a set of at most `LINEAR_SUFFIXES`, scanned in
    /// pattern order.
    AffixSet(AffixSet),
    /// `AffixSet` past `LINEAR_SUFFIXES` (a factored union of names).
    AffixIndex(Box<AffixIndex>),
    /// Everything else: a mini Thompson NFA over the in-segment ops.
    Generic(Box<SegNfa>),
}

/// A prefix and a suffix in one string, `split` bytes of prefix.
#[derive(Debug, Clone)]
struct Affix {
    bytes: Bytes,
    split: u32,
}

impl Affix {
    fn new(prefix: Bytes, suffix: Bytes) -> Self {
        let split = prefix.len() as u32;
        let bytes = match (prefix.is_empty(), suffix.is_empty()) {
            (true, _) => suffix,
            (_, true) => prefix,
            _ => [&prefix[..], &suffix[..]].concat().into_boxed_slice(),
        };
        Affix { bytes, split }
    }

    #[inline]
    fn parts(&self) -> (&[u8], &[u8]) {
        self.bytes.split_at(self.split as usize)
    }

    #[inline]
    fn suffix(&self) -> &[u8] {
        self.parts().1
    }
}

#[derive(Debug, Clone)]
struct AffixSet {
    /// The prefix, then the suffixes.
    parts: Box<[Bytes]>,
    last: ByteSketch,
}

/// A large suffix set: sorted by (length, bytes), ASCII-lowercased
/// when case-insensitive, laid end to end in one allocation, with one
/// `SuffixRun` per length for a binary search by index arithmetic.
#[derive(Debug, Clone)]
struct AffixIndex {
    prefix: Bytes,
    suffixes: Bytes,
    last: ByteSketch,
    /// One run per distinct suffix length, ascending.
    runs: Box<[SuffixRun]>,
}

/// `count` suffixes of `len` bytes each, from byte `start` on.
#[derive(Debug, Clone, Copy)]
struct SuffixRun {
    len: u32,
    start: u32,
    count: u32,
}

/// One path through the graph.
#[derive(Debug, Clone)]
struct Chain {
    /// Its nodes in order; empty for the lone path of a pattern
    /// without a separator-crossing brace, whose nodes are
    /// `0..elems.len()`.
    nodes: Box<[u32]>,
    shape: Shape,
}

/// Where a chain's globstars are, which decides how it is matched.
#[derive(Debug, Clone)]
enum Shape {
    /// None: one element per segment.
    Fixed,
    /// One, at this index: the head and the tail are anchored around
    /// it.
    Single(u32),
    /// Several: the NFA over this chain alone (a union member; a lone
    /// chain with several globstars is the graph, whose NFA serves).
    Many(Box<Nfa<u64>>),
}

/// An NFA over graph positions: one state per position plus the
/// accept state, which is the last.
#[derive(Debug, Clone)]
struct Nfa<B: Bits> {
    states: Box<[State<B>]>,
    /// ε-closure of the start.
    init: B,
    /// States that can still consume ≥ 1 segment on a path to
    /// accept; drives `match_dir`'s prefix bit.
    reach1: B,
}

#[derive(Debug, Clone)]
struct State<B: Bits> {
    /// The closure entered after consuming a segment here (a
    /// globstar's includes itself).
    after: B,
    /// The state's node (the accept slot is unused).
    node: u32,
}

/// The same automaton past `MAX_ROW_BYTES` of rows, stepped sparsely. A position
/// keeps its successors, not a closure row, and the active set holds
/// states and whole positions: "every successor of `p` is entered" is
/// one entry, whose literals are found by one binary search. A step
/// costs what it tests, and the tables grow with the graph, not its
/// square.
#[derive(Debug, Clone)]
struct SparseNfa {
    /// Per position (= its state; the accept state is `nodes.len()`),
    /// its node.
    nodes: Box<[u32]>,
    /// Per position (the start last), its successors: the literal
    /// ones sorted by literal (case-folded when
    /// insensitive), the other positions, and the nullable ones (`**/`)
    /// among those, at `xs[x_at[p]..x_at[p + 1]]`.
    lit_at: Box<[u32]>,
    lits: Box<[u32]>,
    other_at: Box<[u32]>,
    others: Box<[u32]>,
    null_at: Box<[u32]>,
    nulls: Box<[u32]>,
    /// Positions the pattern may end after.
    ends: Wide,
    /// States, and positions with a successor, that can still consume
    /// ≥ 1 segment on a path to accept.
    reach1: Wide,
    succ_reach1: Wide,
}

#[derive(Debug, Clone)]
enum AnyNfa {
    Small(Nfa<u64>),
    // The rarer kinds are boxed, so the common one sets the size.
    Wide(Box<Nfa<Wide>>),
    Sparse(Box<SparseNfa>),
}

impl AnyNfa {
    fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        match self {
            AnyNfa::Small(n) => n.is_match(m, path),
            AnyNfa::Wide(n) => n.is_match(m, path),
            AnyNfa::Sparse(n) => n.is_match(m, path),
        }
    }

    fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        match self {
            AnyNfa::Small(n) => n.match_dir(m, dir_path),
            AnyNfa::Wide(n) => n.match_dir(m, dir_path),
            AnyNfa::Sparse(n) => n.match_dir(m, dir_path),
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
    pub(crate) fn build(program: OpProgram, dot: bool) -> Result<Box<Self>, GlobError> {
        compile::compile(&program.ops, dot, program.case_insensitive).map(Box::new)
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

    /// See `Glob::shape` (test hook).
    pub(crate) fn shape(&self) -> String {
        let elems: Vec<&str> = self
            .elems
            .iter()
            .map(|e| match e {
                Elem::Lit(_) => "L",
                Elem::Wild(w) => match w.kind {
                    WildKind::Affix(_) => "W:affix",
                    WildKind::AffixSet(_) => "W:set",
                    WildKind::AffixIndex(_) => "W:index",
                    WildKind::Generic(_) => "W:nfa",
                },
                Elem::G0 => "G0",
                Elem::G1 => "G1",
            })
            .collect();
        // A run of one kind as `L(200)`.
        let mut runs: Vec<String> = Vec::new();
        let mut i = 0;
        while i < elems.len() {
            let n = elems[i..].iter().take_while(|&&e| e == elems[i]).count();
            runs.push(if n > 1 {
                format!("{}({n})", elems[i])
            } else {
                elems[i].into()
            });
            i += n;
        }
        let nfa = match self.nfa.as_deref() {
            None => "none",
            Some(AnyNfa::Small(_)) => "small",
            Some(AnyNfa::Wide(_)) => "wide",
            Some(AnyNfa::Sparse(_)) => "sparse",
        };
        format!(
            "{} | chains {} | nfa {nfa}",
            runs.join(" "),
            self.chains.len()
        )
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
