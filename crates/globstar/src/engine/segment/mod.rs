//! The segment engine: a pattern as a graph of per-segment elements,
//! matched segment by segment. The design and its thresholds:
//! `references/theory/03-segment-engine.md`.

mod bits;
mod chain;
mod chain_index;
mod graph;
mod graph_nfa;
mod literal_ends;
mod seg_nfa;
mod wild;

use crate::CompileOptions;
use crate::dir_match::DirMatch;
use crate::parse::ast::Node;

use crate::bytes;
use bits::ByteSketch;
use chain::{Chain, Gate, MAX_CHAINS, Shape};
use chain_index::ChainIndex;
use graph::{Elem, Graph};
use graph_nfa::{AnyNfa, any_nfa};
use wild::WildKind;

type Bytes = Box<[u8]>;

#[derive(Debug, Clone)]
pub(crate) struct SegmentMatcher {
    elems: Box<[Elem]>,
    /// The literal set first, when there is one; empty when the NFA
    /// answers alone.
    chains: Box<[Chain]>,
    /// What a lone line does without: the NFA, and the picker of chains.
    union: Option<Box<Union>>,
    /// Literal suffixes one of which every match ends with (empty: none
    /// known), sketched by last byte.
    suffixes: Box<[Bytes]>,
    suffix_last: ByteSketch,
    prefixes: Box<[Bytes]>,
    case_insensitive: bool,
    dot: bool,
}

#[derive(Debug, Clone)]
struct Union {
    nfa: AnyNfa,
    pick: Pick,
}

#[derive(Debug, Clone)]
enum Pick {
    Gates(Box<[Gate]>),
    Index(Box<ChainIndex>),
}

impl SegmentMatcher {
    pub(crate) fn build(mut body: Node, opts: CompileOptions) -> Box<Self> {
        let (dot, ci) = (opts.dot, opts.case_insensitive);
        // The survey, the suffixes and the prefixes read the pattern as
        // written; the graph walk regroups it.
        let survey = graph::survey(body.seq());
        let suffixes = literal_ends::suffixes(&body);
        let prefixes = literal_ends::static_prefixes(&body, survey.weight);
        let mut graph = Graph::build(&mut body, survey, dot, ci);
        let mut chains = if opts.force_nfa {
            Box::default()
        } else {
            graph.chains(ci)
        };
        let index = ChainIndex::new(&chains, &graph, ci);
        // Past `MAX_CHAINS` only the index keeps a call's walks few.
        let set = matches!(chains.first().map(|c| &c.shape), Some(Shape::Set(_)));
        if index.is_none() && chains.len() - usize::from(set) > MAX_CHAINS {
            chains = Box::default();
        }
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

    pub(crate) fn static_prefixes(&self) -> Vec<Vec<u8>> {
        self.prefixes.iter().map(|p| p.to_vec()).collect()
    }

    #[inline]
    pub(crate) fn is_match(&self, path: &[u8]) -> bool {
        if !self.prefilter(path) {
            return false;
        }
        match &self.union {
            None => self.chain_matches(&self.chains[0], path),
            Some(u) if self.chains.is_empty() => u.nfa.is_match(self, path),
            Some(u) => match &u.pick {
                Pick::Index(index) => self.indexed_chain_matches(index, path),
                Pick::Gates(gates) => self.any_chain_matches(gates, path),
            },
        }
    }

    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        match &self.union {
            Some(u) => u.nfa.match_dir(self, dir_path),
            None => self.chain_dir(dir_path),
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
        let (chains, literals) = match self.chains.first().map(|c| &c.shape) {
            Some(Shape::Set(set)) => (self.chains.len() - 1, format!(" | literals {}", set.len())),
            _ => (self.chains.len(), String::new()),
        };
        format!("{} | chains {chains}{literals} | nfa {nfa}", runs.join(" "))
    }

    /// `false`: the path cannot match.
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
