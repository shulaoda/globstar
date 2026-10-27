//! The segment engine: a pattern as a graph of per-segment elements,
//! matched segment by segment. The design and its thresholds:
//! `references/theory/03-segment-engine.md`.

mod bits;
mod chain;
mod graph;
mod graph_nfa;
mod wild;

use crate::CompileOptions;
use crate::dir_match::DirMatch;
use crate::parse::ast::Node;

use crate::engine::bytes;
use bits::ByteSketch;
use chain::{Chain, ChainIndex, Gate, Literals, MAX_CHAINS, Paths, Shape};
use graph::{ACCEPT, Elem, Graph, room};
use graph_nfa::{AnyNfa, any_nfa};
use wild::{SuffixSet, WildKind};

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
        let survey = graph::survey(body.seq());
        let room = room(survey.weight);
        let mut graph = Graph::build(&mut body, survey, dot, ci);
        let suffixes = graph.suffixes();
        let prefixes = graph.static_prefixes(room);
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
                    None if literals.is_none() && chains.is_empty() => Route::Nfa,
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
                Route::Index(index) => self.indexed_chain_matches(index, literals.as_deref(), path),
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
                    set.any_below(dir_path, ci),
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

// The literal ends every match shares, read off the graph: the suffixes
// the prefilter checks and the static prefixes a walker seeds from.

/// Past this many suffixes the prefilter keeps the one they all share.
const MAX_SUFFIX_SET: usize = 16;

/// What an element takes when it is a few literal segments: `prefix`, then
/// one of the set's suffixes, or nothing when there is no set (a literal
/// segment).
#[derive(Clone, Copy)]
struct Exact<'a> {
    prefix: &'a [u8],
    set: Option<&'a SuffixSet>,
}

fn exact(elem: &Elem) -> Option<Exact<'_>> {
    match elem {
        Elem::Lit(lit) => Some(Exact {
            prefix: lit,
            set: None,
        }),
        Elem::Wild(w) => match &w.kind {
            WildKind::Suffixes(set) if !w.variable && w.min_len as usize == set.prefix().len() => {
                Some(Exact {
                    prefix: set.prefix(),
                    set: Some(set),
                })
            }
            _ => None,
        },
        _ => None,
    }
}

impl<'a> Exact<'a> {
    fn ends(self) -> impl Iterator<Item = &'a [u8]> {
        let empty = self.set.is_none_or(SuffixSet::has_empty);
        let set = self.set.into_iter().flat_map(SuffixSet::iter);
        set.chain(empty.then_some(&b""[..]))
    }

    /// The one segment it takes, as prefix and end, if one.
    fn one(self) -> Option<(&'a [u8], &'a [u8])> {
        let mut ends = self.ends();
        match (ends.next(), ends.next()) {
            (Some(end), None) => Some((self.prefix, end)),
            _ => None,
        }
    }
}

/// The longest suffix all of `strings` share.
fn shared_suffix<'a>(mut strings: impl Iterator<Item = &'a [u8]>) -> &'a [u8] {
    let Some(mut shared) = strings.next() else {
        return b"";
    };
    for s in strings {
        let n = shared
            .iter()
            .rev()
            .zip(s.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        shared = &shared[shared.len() - n..];
    }
    shared
}

/// A node the head reader is to visit: where the head before it ends in
/// the buffer, the segment the node before adds (none after a join), and
/// whether the head held a segment before that one.
struct HeadStep<'a> {
    node: u32,
    mark: usize,
    segment: Option<[&'a [u8]; 2]>,
    had: bool,
}

/// The nodes after one: a linear graph's next or `ACCEPT`, or a list.
enum Next<'a> {
    One([u32; 1]),
    Many(&'a [u32]),
}

impl std::ops::Deref for Next<'_> {
    type Target = [u32];
    fn deref(&self) -> &[u32] {
        match self {
            Next::One(one) => one,
            Next::Many(many) => many,
        }
    }
}

impl Graph {
    /// The nodes after `i`, the start being `elems.len()`, and none after
    /// `ACCEPT`.
    fn next(&self, i: usize) -> Next<'_> {
        let n = self.elems.len();
        match &self.edges {
            _ if i > n => Next::Many(&[]),
            Some(edges) => Next::Many(edges.succ(i)),
            None if i == n => Next::One([if n > 0 { 0 } else { ACCEPT }]),
            None => Next::One([if i + 1 < n { i as u32 + 1 } else { ACCEPT }]),
        }
    }

    /// Literal suffixes one of which every match ends with, in pattern
    /// order; empty when nothing is guaranteed. An end that takes whole
    /// literal segments reads back through the literal segments before it,
    /// each the one before the next (§3.3), up to a `**/`, a join or the
    /// start; a segment of any other kind leaves the `/` before.
    fn suffixes(&self) -> Box<[Bytes]> {
        let n = self.elems.len();
        // A node's one predecessor; `NONE` past the start or a join.
        const NONE: u32 = u32::MAX;
        let preds = self.edges.as_ref().map(|_| {
            let mut pred = vec![u32::MAX - 1; n];
            for p in 0..n {
                for &t in &*self.next(p) {
                    if let Some(at) = pred.get_mut(t as usize) {
                        *at = if *at == u32::MAX - 1 { p as u32 } else { NONE };
                    }
                }
            }
            pred
        });
        let pred = |i: usize| match &preds {
            Some(pred) => pred[i] as usize,
            None => i.wrapping_sub(1),
        };
        let mut set: Vec<Bytes> = Vec::with_capacity(1);
        let mut run: Vec<&[u8]> = Vec::new();
        for e in (0..=n).filter(|&p| self.next(p).contains(&ACCEPT)) {
            let Some(el) = self.elems.get(e) else {
                return Box::default();
            };
            // Past `MAX_SUFFIX_SET` only what all share counts, and the
            // ends alone give that.
            let read_back = set.len() <= MAX_SUFFIX_SET;
            // In pattern order, which the prefilter tries first; past
            // `MAX_SUFFIX_SET` the set gives way to what all share.
            let mut push = |parts: &[&[u8]]| {
                let s = parts.concat().into_boxed_slice();
                if set.len() > MAX_SUFFIX_SET || !set.contains(&s) {
                    set.push(s);
                }
            };
            match (exact(el), el) {
                (Some(x), _) if x.ends().nth(MAX_SUFFIX_SET).is_none() => {
                    run.clear();
                    let mut at = e;
                    while read_back && let Some(el) = self.elems.get(pred(at)) {
                        let Some((prefix, end)) = exact(el).and_then(Exact::one) else {
                            if !matches!(el, Elem::G0 | Elem::Join) {
                                run.push(b"/");
                            }
                            break;
                        };
                        run.extend([&b"/"[..], end, prefix]);
                        at = pred(at);
                    }
                    run.reverse();
                    let len = run.len();
                    for end in x.ends() {
                        run.extend([x.prefix, end]);
                        push(&run);
                        run.truncate(len);
                    }
                }
                (Some(x), _) => push(&[shared_suffix(x.ends())]),
                (None, Elem::Wild(w)) => match &w.kind {
                    WildKind::Affix(affix) => push(&[affix.parts().1]),
                    WildKind::Suffixes(set) if !set.has_empty() => {
                        set.iter().for_each(|s| push(&[s]))
                    }
                    WildKind::Pieces(pieces) => push(&[&pieces.last_bytes()]),
                    _ => return Box::default(),
                },
                _ => return Box::default(),
            }
        }
        if set.len() > MAX_SUFFIX_SET {
            set = vec![shared_suffix(set.iter().map(|s| &s[..])).into()];
        }
        if set.iter().any(|s| s.is_empty()) {
            return Box::default();
        }
        set.into_boxed_slice()
    }

    /// The deepest segment-bounded literal head of every path, read from
    /// the start through literal segments, deduplicated; the empty head
    /// ends the reading, as it covers every other. Every byte a head
    /// takes costs a unit of `room`, paid before it is built; a head the
    /// room cannot pay for ends where it is. One buffer holds the head.
    fn static_prefixes(&self, room: usize) -> Box<[Bytes]> {
        let mut stack: Vec<HeadStep> = Vec::new();
        for &t in &*self.next(self.elems.len()) {
            stack.push(HeadStep {
                node: t,
                mark: 0,
                segment: None,
                had: false,
            });
        }
        let (mut head, mut spent, mut out) = (Vec::new(), 0usize, Vec::with_capacity(1));
        while let Some(HeadStep {
            node: t,
            mark,
            segment,
            had,
        }) = stack.pop()
        {
            head.truncate(mark);
            if let Some([prefix, end]) = segment {
                if had {
                    head.push(b'/');
                }
                head.extend_from_slice(prefix);
                head.extend_from_slice(end);
            }
            let has = had || segment.is_some();
            let next = self.next(t as usize);
            // A join takes nothing and passes the head on.
            let (join, ways) = match self.elems.get(t as usize) {
                Some(Elem::Join) => (
                    true,
                    Some(Exact {
                        prefix: b"",
                        set: None,
                    }),
                ),
                el => (false, el.and_then(exact)),
            };
            let cost = ways.map_or(0, |x| {
                let each = |end: &[u8]| head.len() + 1 + x.prefix.len() + end.len();
                x.ends().map(each).sum::<usize>() * next.len() + 1
            });
            let Some(x) = ways.filter(|_| spent + cost <= room) else {
                let mut to = head.len();
                while to > 0 && head[to - 1] == b'/' {
                    to -= 1;
                }
                // The empty head covers every other.
                if to == 0 {
                    return Box::new([Bytes::default()]);
                }
                out.push(Bytes::from(&head[..to]));
                continue;
            };
            spent += cost;
            for end in x.ends() {
                for &u in &*next {
                    stack.push(HeadStep {
                        node: u,
                        mark: head.len(),
                        segment: (!join).then_some([x.prefix, end]),
                        had: has,
                    });
                }
            }
        }
        (if out.len() > 1 { dedupe(out) } else { out }).into_boxed_slice()
    }
}

/// Drops the prefixes a shorter one covers; an empty one covers all. In
/// byte order a head's prefixes come before it: `top` walks down the kept
/// ones that are prefixes of the current head (`up` links each to the one
/// below), and the first alone may cover it, since one below would have
/// covered that one.
fn dedupe(mut prefixes: Vec<Bytes>) -> Vec<Bytes> {
    prefixes.sort_unstable();
    if prefixes[0].is_empty() {
        return vec![Bytes::default()];
    }
    let mut kept: Vec<Bytes> = Vec::new();
    let mut up: Vec<usize> = Vec::new();
    let mut top = usize::MAX;
    for p in prefixes {
        while top != usize::MAX && !p.starts_with(&kept[top]) {
            top = up[top];
        }
        if top != usize::MAX && (p.len() == kept[top].len() || p[kept[top].len()] == b'/') {
            continue;
        }
        up.push(top);
        top = kept.len();
        kept.push(p);
    }
    // Stable: a length's heads stay in byte order.
    kept.sort_by_key(|p| p.len());
    kept
}
