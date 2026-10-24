//! The segment engine: a pattern becomes a graph of per-segment
//! elements (literals, in-segment wildcard matchers, globstars) and a
//! path is matched segment by segment. A path through the graph with
//! at most one globstar, or two around a block, is matched by anchored
//! compares, the literal-only paths by one search of a sorted set, and
//! a union of many paths asks only those its path's segments pick;
//! everything else runs an NFA whose active set grows with the
//! pattern, so no pattern is too big for it. A segment a brace cuts is
//! a run of glued pieces, which one in-segment NFA matches together.
//! The graph is linear in the pattern: every pattern compiles.
//!
//! `elem` holds the graph's elements, `graph` builds the graph,
//! `chain` and `graph_nfa` match over it, `chain_index` picks a big
//! union's chains, `wild` (with `pieces` and `seg_nfa`) matches inside
//! a segment, `seg_nfa` also across glued pieces, `facts` reads the
//! literal facts off the AST, `bits` holds the bit sets (the NFAs', the
//! byte sketches); the byte comparisons are `crate::bytes`. The layers, the
//! thresholds that pick them and what each buys:
//! `references/theory/02-segment-engine.md`.

mod bits;
mod chain;
mod chain_index;
mod elem;
mod facts;
mod graph;
mod graph_nfa;
mod pieces;
mod seg_nfa;
mod wild;

use crate::CompileOptions;
use crate::dir_match::DirMatch;
use crate::parse::ast::Node;

use crate::bytes;
use bits::ByteSketch;
use chain::{Chain, Gate, MAX_CHAINS, Shape};
use chain_index::ChainIndex;
use elem::Elem;
use graph::Graph;
use graph_nfa::{AnyNfa, any_nfa};
use wild::WildKind;

/// A byte string the matcher keeps: literals, affixes, suffixes,
/// static prefixes.
type Bytes = Box<[u8]>;

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
    pub(crate) fn build(mut body: Node, opts: CompileOptions) -> Box<Self> {
        let (dot, ci) = (opts.dot, opts.case_insensitive);
        // The facts read the pattern as written; the graph walk
        // regroups it.
        let survey = graph::survey(body.seq());
        let suffixes = facts::suffixes(&body);
        let prefixes = facts::static_prefixes(&body, survey.weight);
        let mut graph = Graph::build(&mut body, survey, dot, ci);
        // The test hook keeps no chain.
        let mut chains = if opts.force_nfa {
            Box::default()
        } else {
            graph.chains(ci)
        };
        let index = ChainIndex::new(&chains, &graph, ci);
        // Past `MAX_CHAINS` only the index keeps a call's walks few;
        // without one the NFA serves.
        let set = matches!(chains.first().map(|c| &c.shape), Some(Shape::Set(_)));
        if index.is_none() && chains.len() - usize::from(set) > MAX_CHAINS {
            chains = Box::default();
        }
        // A lone line serves `match_dir` too; anything else needs the NFA.
        let union = match &chains[..] {
            [c] if matches!(c.shape, Shape::Line(_)) => None,
            _ => Some(Box::new(Union {
                nfa: any_nfa(&graph.elems, ci, graph.edges.as_ref(), graph.glue.take()),
                pick: match index {
                    Some(index) => Pick::Index(index),
                    None => Pick::Gates(chains.iter().map(|c| c.gate(&graph.elems, ci)).collect()),
                },
            })),
        };
        Box::new(SegmentMatcher {
            suffix_last: ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci),
            suffixes,
            chains,
            union,
            elems: graph.elems.into_boxed_slice(),
            prefixes,
            case_insensitive: ci,
            dot,
        })
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
                    WildKind::Pieces(_) => "W:pieces",
                    WildKind::Generic(_) => "W:nfa",
                },
                Elem::G0 => "G0",
                Elem::G1 => "G1",
                Elem::Join => "J",
                Elem::Piece(_) => "P",
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
        let ends = |s: &Bytes| bytes::ends_with_pattern(path, s, self.case_insensitive);
        match &self.suffixes[..] {
            [] => true,
            [one] => ends(one),
            set => self.suffix_last.may_end(path) && set.iter().any(ends),
        }
    }

    #[inline]
    fn lit_eq(&self, lit: &[u8], seg: &[u8]) -> bool {
        lit.len() == seg.len() && self.affix_eq(lit, seg)
    }

    #[inline(always)]
    fn affix_eq(&self, part: &[u8], seg_part: &[u8]) -> bool {
        bytes::affix_eq(part, seg_part, self.case_insensitive)
    }
}
