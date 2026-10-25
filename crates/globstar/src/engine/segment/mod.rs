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
use chain::{Chain, Gate, Literals, MAX_CHAINS, Paths, Shape};
use chain_index::ChainIndex;
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
struct Union {
    /// The literal paths, searched first.
    literals: Option<Box<Literals>>,
    nfa: AnyNfa,
    route: Route,
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
        let suffixes = literal_ends::suffixes(&body);
        let prefixes = literal_ends::static_prefixes(&body, survey.weight);
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
        let union = match (&literals, &chains[..]) {
            (None, [c]) if matches!(c.shape, Shape::Line(_)) => None,
            _ => {
                let route = match index {
                    Some(index) => Route::Index(index),
                    None if literals.is_none() && chains.is_empty() => Route::Nfa,
                    None => Route::Gates(chains.iter().map(|c| c.gate(&graph.elems, ci)).collect()),
                };
                Some(Box::new(Union {
                    literals,
                    nfa: any_nfa(&graph.elems, ci, graph.edges.as_ref(), graph.glue.take()),
                    route,
                }))
            }
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
            Some(u) => match &u.route {
                Route::Nfa => u.nfa.is_match(self, path),
                Route::Index(index) => {
                    self.indexed_chain_matches(index, u.literals.as_deref(), path)
                }
                Route::Gates(gates) => self.any_chain_matches(gates, u.literals.as_deref(), path),
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
        let union = self.union.as_deref();
        let nfa = match union.map(|u| &u.nfa) {
            None => "none",
            Some(AnyNfa::Small(_)) => "small",
            Some(AnyNfa::Wide(_)) => "wide",
            Some(AnyNfa::Sparse(_)) => "sparse",
        };
        let literals = union
            .and_then(|u| u.literals.as_ref())
            .map_or(String::new(), |set| format!(" | literals {}", set.len()));
        format!(
            "{} | chains {}{literals} | nfa {nfa}",
            runs.join(" "),
            self.chains.len()
        )
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
