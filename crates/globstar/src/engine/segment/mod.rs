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
use crate::parse::ast::{Node, ends_in_globstar};

use crate::bytes::{self, has_escaped_sep};
use bits::ByteSketch;
use chain::{Chain, ChainIndex, Gate, Literals, MAX_CHAINS, Paths, Shape};
use graph::{Elem, Graph, node_weight};
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
        let suffixes = suffixes(&body);
        let prefixes = static_prefixes(&body, survey.weight);
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

// The literal ends every match shares, read off the AST: the suffixes
// the prefilter checks and the static prefixes a walker seeds from.

/// A pattern that ends in a brace of more branches gets no prefilter.
const MAX_SUFFIX_SET: usize = 16;

/// How deep braces may nest along one head, the budget every pattern
/// gets for reading heads, and how many budgets of bytes recording them
/// may cost.
const MAX_HEAD_DEPTH: usize = 64;
const HEAD_BUDGET_FLOOR: usize = 4096;
const HEAD_COPY_FACTOR: usize = 64;

/// Literal suffixes one of which every match ends with: the trailing
/// literal run, or one per branch of a trailing brace. Empty when nothing
/// is guaranteed.
fn suffixes(body: &Node) -> Box<[Bytes]> {
    let nodes = body.seq();
    let run = trailing(nodes, false);
    if run.is_empty() {
        return suffix_set(nodes);
    }
    let mut acc = Vec::with_capacity(run.iter().map(node_weight).sum());
    push_run(run, &mut acc);
    Box::new([acc.into_boxed_slice()])
}

/// The literal/separator run that ends `nodes`. The `/` of a `**/` is
/// not part of it, nor one a brace that may be empty parts from a `**`:
/// `**/b` and `**{,a}/b` match `b` alone.
fn trailing(nodes: &[Node], after_globstar: bool) -> &[Node] {
    let mut from = nodes.len();
    while from > 0 {
        match &nodes[from - 1] {
            Node::Literal(_) => {}
            Node::Separator if !ends_in_globstar(&nodes[..from - 1], after_globstar) => {}
            _ => break,
        }
        from -= 1;
    }
    &nodes[from..]
}

fn push_run(run: &[Node], acc: &mut Vec<u8>) {
    for node in run {
        match node {
            Node::Literal(bytes) => acc.extend_from_slice(bytes),
            _ => acc.push(b'/'),
        }
    }
}

/// One suffix per branch of a trailing brace, or none when a branch
/// guarantees nothing.
fn suffix_set(nodes: &[Node]) -> Box<[Bytes]> {
    let branches = match nodes.last() {
        Some(Node::Brace(branches)) if branches.len() <= MAX_SUFFIX_SET => branches,
        _ => return Box::default(),
    };
    let before = &nodes[..nodes.len() - 1];
    let mut common = Vec::new();
    push_run(trailing(before, false), &mut common);
    let after_globstar = ends_in_globstar(before, false);
    let mut set = Vec::with_capacity(branches.len());
    let mut acc = Vec::new();
    for branch in branches {
        acc.clear();
        // The common tail joins a branch only when the branch is all
        // literal; otherwise it would claim more than the branch does.
        if branch
            .seq()
            .iter()
            .all(|n| matches!(n, Node::Literal(_) | Node::Separator))
        {
            acc.extend_from_slice(&common);
        }
        push_run(trailing(branch.seq(), after_globstar), &mut acc);
        if acc.is_empty() {
            return Box::default();
        }
        set.push(Bytes::from(&acc[..]));
    }
    set.into_boxed_slice()
}

/// The deepest segment-bounded literal head of each brace expansion,
/// deduplicated. Every byte read and branch taken costs one unit of a
/// budget; past it, or past `MAX_HEAD_DEPTH`, a head ends at its last
/// whole segment, a shorter prefix that still holds. Past
/// `HEAD_COPY_FACTOR` budgets of bytes copied into heads, the reading
/// starts again at the depth limit, which gives the one head before the
/// first brace.
fn static_prefixes(body: &Node, weight: usize) -> Box<[Bytes]> {
    let read_at = |depth| {
        let mut heads = Heads {
            spent: 0,
            budget: HEAD_BUDGET_FLOOR + 2 * weight,
            copied: 0,
            out: Vec::with_capacity(1),
        };
        heads.read(body.seq(), None, &mut Vec::new(), 0, depth);
        heads
    };
    let mut heads = read_at(0);
    if heads.copied > HEAD_COPY_FACTOR.saturating_mul(heads.budget) {
        heads = read_at(MAX_HEAD_DEPTH);
    }
    let out = if heads.out.len() > 1 {
        dedupe(heads.out)
    } else {
        heads.out
    };
    out.into_boxed_slice()
}

/// What follows the nodes being read: the rest of an enclosing sequence.
struct Then<'a> {
    nodes: &'a [Node],
    next: Option<&'a Then<'a>>,
}

struct Heads {
    spent: usize,
    budget: usize,
    copied: usize,
    out: Vec<Bytes>,
}

impl Heads {
    fn afford(&mut self, n: usize) -> bool {
        if self.spent + n > self.budget {
            return false;
        }
        self.spent += n;
        true
    }

    /// An empty head covers every other one.
    fn covered(&self) -> bool {
        self.out.last().is_some_and(|h| h.is_empty())
    }

    /// Read `nodes`, then `then`, after `acc` (whose last whole segment
    /// ends at `boundary`), and record the head each expansion reaches.
    fn read<'a>(
        &mut self,
        mut nodes: &'a [Node],
        mut then: Option<&'a Then<'a>>,
        acc: &mut Vec<u8>,
        mut boundary: usize,
        depth: usize,
    ) {
        let mark = acc.len();
        let end = loop {
            let Some((node, rest)) = nodes.split_first() else {
                match then {
                    Some(t) => {
                        nodes = t.nodes;
                        then = t.next;
                        continue;
                    }
                    None => break acc.len(),
                }
            };
            match node {
                // An expansion through an escaped `\` on Windows matches
                // nothing (GLOB_SPEC §4.2): no head to seed.
                Node::Literal(bytes) if has_escaped_sep(bytes) => {
                    acc.truncate(mark);
                    return;
                }
                Node::Literal(bytes) if self.afford(bytes.len()) => acc.extend_from_slice(bytes),
                Node::Separator if self.afford(1) => {
                    acc.push(b'/');
                    boundary = acc.len();
                }
                Node::Brace(bs) if depth < MAX_HEAD_DEPTH && self.afford(bs.len()) => {
                    let after = Then {
                        nodes: rest,
                        next: then,
                    };
                    for branch in bs {
                        if self.covered() {
                            break;
                        }
                        self.read(branch.seq(), Some(&after), acc, boundary, depth + 1);
                    }
                    acc.truncate(mark);
                    return;
                }
                // Every branch opening a new segment leaves what came
                // before whole.
                Node::Brace(bs)
                    if bs
                        .iter()
                        .all(|b| matches!(b.seq().first(), Some(Node::Separator))) =>
                {
                    break acc.len();
                }
                _ => break boundary,
            }
            nodes = rest;
        };
        let mut head = &acc[..end];
        while let [rest @ .., b'/'] = head {
            head = rest;
        }
        self.copied += acc.len();
        if self.copied > HEAD_COPY_FACTOR.saturating_mul(self.budget) {
            self.out = vec![Bytes::default()];
        } else {
            self.out.push(Bytes::from(head));
        }
        acc.truncate(mark);
    }
}

/// Drops the prefixes a shorter one covers; an empty one covers all.
fn dedupe(mut prefixes: Vec<Bytes>) -> Vec<Bytes> {
    prefixes.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    prefixes.dedup();
    let mut kept: Vec<Bytes> = Vec::new();
    for p in prefixes {
        if p.is_empty() {
            return vec![p];
        }
        let covered = (0..p.len()).filter(|&j| p[j] == b'/').any(|j| {
            kept.binary_search_by(|k| k.len().cmp(&j).then_with(|| k[..].cmp(&p[..j])))
                .is_ok()
        });
        if !covered {
            kept.push(p);
        }
    }
    kept
}
