//! The literal ends every match shares, read off the AST: the suffixes
//! the prefilter checks and the static prefixes a walker seeds from.

use crate::bytes::has_escaped_sep;
use crate::parse::ast::{Node, ends_in_globstar};

use super::Bytes;
use super::graph::node_weight;

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
pub(super) fn suffixes(body: &Node) -> Box<[Bytes]> {
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
pub(super) fn static_prefixes(body: &Node, weight: usize) -> Box<[Bytes]> {
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
