//! The segment engine: a pattern becomes a graph of per-segment
//! elements (literals, in-segment wildcard matchers, globstars) and a
//! path is matched segment by segment. A path through the graph with
//! at most one globstar is matched by anchored compares; everything
//! else runs an NFA whose active set grows with the pattern, so no
//! pattern is too big for it. Only braces whose segments multiply are
//! refused, by the compile walk (`GlobError::TooComplex`).
//!
//! `graph` builds the graph, `chain` and `nfa` match over it, `wild`
//! (with `seg_nfa`) matches inside a segment, `facts` reads the literal
//! facts off the AST, `bits` holds the bit sets both NFAs use.

mod bits;
mod chain;
mod facts;
mod graph;
mod nfa;
mod seg_nfa;
mod wild;

use crate::dir_match::DirMatch;
use crate::error::GlobError;
use crate::parse::ast::Node;

use super::pat_byte_eq;
use chain::Chain;
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
    pub(crate) fn build(body: &Node, dot: bool, ci: bool) -> Result<Box<Self>, GlobError> {
        let graph = Graph::build(body, dot, ci)?;
        let chains = graph.chains();
        // A lone chain serves `match_dir` too; anything else needs the NFA.
        let nfa = match &chains[..] {
            [_] => None,
            _ => Some(Box::new(any_nfa(&graph.elems, ci, graph.edges.as_ref()))),
        };
        let suffixes = facts::suffixes(body);
        Ok(Box::new(SegmentMatcher {
            suffix_last: ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci),
            suffixes,
            chains,
            nfa,
            elems: graph.elems.into_boxed_slice(),
            prefixes: facts::static_prefixes(body, graph.weight),
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
        match (&self.chains[..], &self.nfa) {
            ([], Some(nfa)) => nfa.is_match(self, path),
            ([_], _) => self.chain_matches(0, path),
            _ => self.any_chain_matches(path),
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
            None => {
                let (m, nfa) = chain_nfa(self);
                nfa.is_match(&m, path)
            }
        }
    }

    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        match &self.nfa {
            Some(nfa) => nfa.match_dir(self, dir_path),
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

    /// Byte-wise, so short affixes stay inline rather than calling
    /// `bcmp`.
    #[inline(always)]
    fn affix_eq(&self, part: &[u8], seg_part: &[u8]) -> bool {
        debug_assert_eq!(part.len(), seg_part.len());
        if self.case_insensitive {
            part.iter()
                .zip(seg_part)
                .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
        } else {
            part.iter().zip(seg_part).all(|(a, b)| a == b)
        }
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
