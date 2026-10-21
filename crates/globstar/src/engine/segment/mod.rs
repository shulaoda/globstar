//! The segment engine: a pattern becomes a graph of per-segment
//! elements (literals, in-segment wildcard matchers, globstars) and a
//! path is matched segment by segment. A path through the graph with
//! at most one globstar, or two around a block, is matched by anchored
//! compares, the literal-only paths by one search of a sorted set;
//! everything else runs an NFA whose active set grows with the
//! pattern, so no pattern is too big for it. Only braces whose segments
//! multiply are refused, by the compile walk (`GlobError::TooComplex`).
//!
//! `graph` builds the graph, `chain` and `nfa` match over it, `wild`
//! (with `seg_nfa`) matches inside a segment, `facts` reads the literal
//! facts off the AST, `bits` holds the bit sets both NFAs use.

mod bits;
mod chain;
mod chain_index;
mod facts;
mod graph;
mod nfa;
mod seg_nfa;
mod wild;

use crate::dir_match::DirMatch;
use crate::error::GlobError;
use crate::parse::ast::Node;

use super::pat_byte_eq;
use chain::{Chain, Gate, Shape};
use chain_index::ChainIndex;
use graph::Graph;
use nfa::{AnyNfa, any_nfa, chain_nfa};
use wild::{ByteSketch, Wild, WildKind};

/// A byte string the matcher keeps: literals, affixes, suffixes,
/// static prefixes.
type Bytes = Box<[u8]>;

/// The `min_len` of a segment no path segment can fill: one with an
/// escaped `\` on Windows, where a segment never holds a separator, or
/// an in-segment NFA that accepts nothing.
const NEVER: u32 = u32::MAX;

/// Does `lit` hold a separator byte (an escaped `\` on Windows)? A
/// literal never holds a `/`, so elsewhere it never does.
fn has_sep(lit: &[u8]) -> bool {
    cfg!(windows) && lit.iter().any(|&b| std::path::is_separator(char::from(b)))
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
        !matches!(self, Elem::Wild(w) if w.min_len == NEVER)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SegmentMatcher {
    /// The graph's nodes, in emission order.
    elems: Box<[Elem]>,
    /// The paths through the graph, tried in order (the literal ones as
    /// one set, first); none past the chain module's caps.
    chains: Box<[Chain]>,
    /// What a lone line does without: the NFA over the graph, which
    /// serves `match_dir` (and `is_match` when the chains cannot), and
    /// what picks the chains to try.
    union: Option<Box<Union>>,
    /// Prefilter: literal suffixes one of which every match ends
    /// with (empty: none known), sketched by last byte.
    suffixes: Box<[Bytes]>,
    suffix_last: ByteSketch,
    /// Literal path prefixes a walker can seed traversal from.
    prefixes: Box<[Bytes]>,
    case_insensitive: bool,
    dot: bool,
}

/// What a matcher keeps beyond a lone line: the NFA over the graph, and
/// what picks the chains `is_match` tries.
#[derive(Debug, Clone)]
struct Union {
    nfa: AnyNfa,
    pick: Pick,
}

/// A few chains are all tried, each after its gate; many, only those the
/// chain index picks.
#[derive(Debug, Clone)]
enum Pick {
    Gates(Box<[Gate]>),
    Index(Box<ChainIndex>),
}

impl SegmentMatcher {
    pub(crate) fn build(mut body: Node, dot: bool, ci: bool) -> Result<Box<Self>, GlobError> {
        // The facts read the pattern as written; the graph walk
        // regroups it.
        let survey = graph::survey(body.seq());
        let suffixes = facts::suffixes(&body);
        let prefixes = facts::static_prefixes(&body, survey.weight);
        let graph = Graph::build(&mut body, survey, dot, ci)?;
        let chains = graph.chains(ci);
        // A lone line serves `match_dir` too; anything else needs the NFA.
        let union = match &chains[..] {
            [c] if matches!(c.shape, Shape::Line(_)) => None,
            _ => Some(Box::new(Union {
                nfa: any_nfa(&graph.elems, ci, graph.edges.as_ref()),
                pick: match ChainIndex::new(&chains, &graph.elems, &graph.keys, ci) {
                    Some(index) => Pick::Index(index),
                    None => Pick::Gates(chains.iter().map(|c| c.gate(&graph.elems, ci)).collect()),
                },
            })),
        };
        Ok(Box::new(SegmentMatcher {
            suffix_last: ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci),
            suffixes,
            chains,
            union,
            elems: graph.elems.into_boxed_slice(),
            prefixes,
            case_insensitive: ci,
            dot,
        }))
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
        match &self.union {
            // A lone line.
            None => self.chain_matches(&self.chains[0], path),
            Some(u) if self.chains.is_empty() => u.nfa.is_match(self, path),
            Some(u) => match &u.pick {
                Pick::Index(index) => self.indexed_chain_matches(index, path),
                Pick::Gates(gates) => self.any_chain_matches(gates, path),
            },
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
                    WildKind::Suffixes(_) => "W:set",
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
        let nfa = match self.union.as_deref().map(|u| &u.nfa) {
            None => "none",
            Some(AnyNfa::Small(_)) => "small",
            Some(AnyNfa::Wide(_)) => "wide",
            Some(AnyNfa::Sparse(_)) => "sparse",
        };
        // The literal paths' set is counted apart from the chains.
        let (chains, literals) = match self.chains.first().map(|c| &c.shape) {
            Some(Shape::Set(set)) => (self.chains.len() - 1, format!(" | literals {}", set.len())),
            _ => (self.chains.len(), String::new()),
        };
        format!("{} | chains {chains}{literals} | nfa {nfa}", runs.join(" "))
    }

    /// `is_match` through an NFA only (test hook): the matcher's own,
    /// or one laid out over the lone chain it kept instead.
    pub(crate) fn is_match_nfa(&self, path: &[u8]) -> bool {
        if !self.prefilter(path) {
            return false;
        }
        match &self.union {
            Some(u) => u.nfa.is_match(self, path),
            None => {
                let (m, nfa) = chain_nfa(self);
                nfa.is_match(&m, path)
            }
        }
    }

    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        match &self.union {
            Some(u) => u.nfa.match_dir(self, dir_path),
            None => self.chain_dir(dir_path),
        }
    }
}

impl SegmentMatcher {
    /// The suffix prefilter: `false` means the path cannot match.
    #[inline]
    fn prefilter(&self, path: &[u8]) -> bool {
        match &self.suffixes[..] {
            [] => true,
            [one] => self.ends_with(path, one),
            set => self.suffix_last.may_end(path) && set.iter().any(|s| self.ends_with(path, s)),
        }
    }

    /// Suffix test, last byte first: that is where extensions differ.
    #[inline]
    fn ends_with(&self, path: &[u8], suffix: &[u8]) -> bool {
        path.len() >= suffix.len()
            && suffix
                .iter()
                .zip(&path[path.len() - suffix.len()..])
                .rev()
                .all(|(&s, &p)| pat_byte_eq(s, p, self.case_insensitive))
    }

    #[inline]
    fn lit_eq(&self, lit: &[u8], seg: &[u8]) -> bool {
        lit.len() == seg.len() && self.affix_eq(lit, seg)
    }

    #[inline(always)]
    fn affix_eq(&self, part: &[u8], seg_part: &[u8]) -> bool {
        wild::affix_eq(part, seg_part, self.case_insensitive)
    }
}

/// Index just past the segment that starts at `pos`.
#[inline]
fn seg_end(path: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i < path.len() && !std::path::is_separator(path[i] as char) {
        i += 1;
    }
    i
}

/// Start of the segment that ends at `end`.
#[inline]
fn seg_start(path: &[u8], end: usize) -> usize {
    let mut s = end;
    while s > 0 && !std::path::is_separator(path[s - 1] as char) {
        s -= 1;
    }
    s
}
