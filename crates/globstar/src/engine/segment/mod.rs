//! The segment engine: a pattern as a graph of per-segment elements,
//! matched segment by segment. The design and its thresholds:
//! `references/theory/03-segment-engine.md`.

mod bits;
mod chain;
mod ends;
mod graph;
mod graph_nfa;
mod seg_nfa;
mod wild;

use crate::CompileOptions;
use crate::dir_match::DirMatch;
use crate::parse::ast::Node;

use crate::engine::bytes;
use bits::ByteSketch;
use chain::{Chain, ChainIndex, Gate, Literals, MAX_CHAINS, Paths, Shape};
use graph::{Elem, Graph};
use graph_nfa::{AnyNfa, any_nfa};
use wild::WildKind;

type Bytes = Box<[u8]>;

#[derive(Debug, Clone)]
pub(crate) struct SegmentMatcher {
    elems: Box<[Elem]>,
    /// The wildcard paths; empty when the NFA answers alone.
    chains: Box<[Chain]>,
    /// What a lone line does without.
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
enum Union {
    /// Literal paths only: the sorted set answers every call.
    Set(Box<Literals>),
    Graph {
        /// The literal paths, searched first.
        literals: Option<Box<Literals>>,
        nfa: AnyNfa,
        route: Route,
    },
}

/// How a union's call finds its chain.
#[derive(Debug, Clone)]
enum Route {
    /// No chains and no literals: the NFA alone.
    Nfa,
    Gates(Box<[Gate]>),
    Index(Box<ChainIndex>),
}

impl SegmentMatcher {
    pub(crate) fn build(mut body: Node, opts: CompileOptions) -> Box<Self> {
        let (dot, ci) = (opts.dot, opts.case_insensitive);
        // The survey, the suffixes and the prefixes read the pattern as
        // written; the graph walk regroups it.
        let survey = graph::survey(body.seq());
        let suffixes = ends::suffixes(&body);
        let prefixes = ends::static_prefixes(&body, survey.weight);
        let mut graph = Graph::build(&mut body, survey, dot, ci);
        let paths = if opts.force_nfa {
            None
        } else {
            graph.paths(ci)
        };
        let Paths {
            mut literals,
            mut chains,
        } = paths.unwrap_or_default();
        let index = ChainIndex::new(literals.as_deref(), &chains, &graph, ci);
        // Past `MAX_CHAINS` only the index keeps a call's walks few.
        if index.is_none() && chains.len() > MAX_CHAINS {
            literals = None;
            chains = Box::default();
        }
        let union = match (literals, &chains[..]) {
            (None, [c]) if matches!(c.shape, Shape::Line(_)) => None,
            (Some(set), []) => Some(Box::new(Union::Set(set))),
            (literals, _) => {
                let route = match index {
                    Some(index) => Route::Index(index),
                    None if chains.is_empty() => Route::Nfa,
                    None => Route::Gates(chains.iter().map(|c| c.gate(&graph.elems, ci)).collect()),
                };
                Some(Box::new(Union::Graph {
                    literals,
                    nfa: any_nfa(&graph.elems, ci, graph.edges.as_ref(), graph.glue.take()),
                    route,
                }))
            }
        };
        let elems = match union.as_deref() {
            Some(Union::Set(_)) => Box::default(),
            _ => graph.elems.into_boxed_slice(),
        };
        Box::new(SegmentMatcher {
            suffix_last: ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci),
            suffixes,
            chains,
            union,
            elems,
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
        match self.union.as_deref() {
            None => self.chain_matches(&self.chains[0], path),
            Some(Union::Set(set)) => set.contains(path, self.case_insensitive),
            Some(Union::Graph {
                literals,
                nfa,
                route,
            }) => match route {
                Route::Nfa => nfa.is_match(self, path),
                Route::Index(index) => {
                    self.indexed_chain_matches(index, literals.as_deref(), nfa, path)
                }
                Route::Gates(gates) => self.any_chain_matches(gates, literals.as_deref(), path),
            },
        }
    }

    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        match self.union.as_deref() {
            None => self.chain_dir(dir_path),
            Some(Union::Set(set)) => {
                let ci = self.case_insensitive;
                DirMatch::new(
                    set.contains(dir_path, ci),
                    dir_path.is_empty() || set.any_below(dir_path, ci),
                    false,
                )
            }
            Some(Union::Graph { nfa, .. }) => nfa.match_dir(self, dir_path),
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
        let (literals, nfa) = match self.union.as_deref() {
            None => (None, "none"),
            Some(Union::Set(set)) => (Some(set.len()), "none"),
            Some(Union::Graph { literals, nfa, .. }) => (
                literals.as_ref().map(|set| set.len()),
                match nfa {
                    AnyNfa::Small(_) => "small",
                    AnyNfa::Wide(_) => "wide",
                    AnyNfa::Sparse(_) => "sparse",
                },
            ),
        };
        let mut parts = Vec::new();
        if !runs.is_empty() {
            parts.push(runs.join(" "));
        }
        parts.push(format!("chains {}", self.chains.len()));
        if let Some(n) = literals {
            parts.push(format!("literals {n}"));
        }
        parts.push(format!("nfa {nfa}"));
        parts.join(" | ")
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
