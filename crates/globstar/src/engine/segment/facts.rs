//! Literal facts read off the AST at compile time: the suffix
//! prefilter and the static prefixes a walker seeds from.

use crate::ast::Node;

use super::compile::{len_then_bytes, node_weight};
use super::{Bytes, has_sep};

/// Largest suffix set worth scanning before the engine runs; a
/// bigger alternation gets no prefilter (the engine indexes it).
const MAX_SUFFIX_SET: usize = 16;

/// Literal suffixes one of which every match ends with: the trailing
/// literal/separator run (separators as `/`), or when the pattern ends
/// in a brace, one per branch. Empty when nothing is guaranteed.
pub(super) fn suffixes(body: &Node) -> Box<[Bytes]> {
    let nodes = body.seq();
    let run = trailing(nodes);
    if run.is_empty() {
        return suffix_set(nodes);
    }
    let mut acc = Vec::with_capacity(run.iter().map(node_weight).sum());
    push_run(run, &mut acc);
    Box::new([acc.into_boxed_slice()])
}

/// The literal/separator run that ends `nodes`. The `/` of a `**/` is
/// not part of it: `**/b` matches `b` alone.
fn trailing(nodes: &[Node]) -> &[Node] {
    let mut from = nodes.len();
    while from > 0 {
        match &nodes[from - 1] {
            Node::Literal(_) => {}
            Node::Separator
                if !nodes[..from - 1]
                    .last()
                    .is_some_and(Node::ends_with_globstar) => {}
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

/// When the pattern ends in a brace: one guaranteed suffix per branch,
/// or none when a branch guarantees nothing.
fn suffix_set(nodes: &[Node]) -> Box<[Bytes]> {
    let branches = match nodes.last() {
        Some(Node::Brace(branches)) if branches.len() <= MAX_SUFFIX_SET => branches,
        _ => return Box::default(),
    };
    let mut common = Vec::new();
    push_run(trailing(&nodes[..nodes.len() - 1]), &mut common);
    let mut set = Vec::with_capacity(branches.len());
    let mut acc = Vec::new();
    for branch in branches {
        acc.clear();
        let branch = branch.seq();
        // Gluing the common tail to a branch that is not all-literal
        // would claim more than the branch guarantees.
        if branch
            .iter()
            .all(|n| matches!(n, Node::Literal(_) | Node::Separator))
        {
            acc.extend_from_slice(&common);
        }
        push_run(trailing(branch), &mut acc);
        if acc.is_empty() {
            return Box::default();
        }
        set.push(Bytes::from(&acc[..]));
    }
    set.into_boxed_slice()
}

/// How deep brace expansions may nest along one static prefix, and the
/// part of its budget that does not depend on the pattern.
const MAX_HEAD_DEPTH: usize = 64;
const HEAD_BUDGET_FLOOR: usize = 4096;

/// The deepest segment-bounded literal head of each expansion,
/// deduplicated, sorted by (length, bytes). A brace in the head is taken
/// branch by branch with what follows it (`{a,b}/c` gives `a/c` and
/// `b/c`), so a factored union keeps its members' heads.
///
/// Every byte read and every branch taken costs one unit of a budget of
/// `HEAD_BUDGET_FLOOR` plus twice the pattern's weight (`node_weight`,
/// only worked out once the floor is spent): reading each node once
/// costs at most its weight, and re-reading what follows a brace for
/// each branch is what the rest pays for. Past the budget, or past
/// `MAX_HEAD_DEPTH` nested braces, the head ends at its last whole
/// segment, a shorter prefix that still holds: `{a,b}{c,d}…` stops after
/// a few braces instead of doubling, and a brace before a long literal
/// copies it a few times, not once per branch.
pub(super) fn static_prefixes(body: &Node) -> Box<[Bytes]> {
    let nodes = body.seq();
    let mut heads = Heads {
        root: nodes,
        spent: 0,
        limit: HEAD_BUDGET_FLOOR,
        weighed: false,
        out: Vec::with_capacity(1),
    };
    heads.read(nodes, None, &mut Vec::new(), 0, 0);
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

struct Heads<'r> {
    root: &'r [Node],
    spent: usize,
    /// The floor until it is spent, then the floor plus twice the weight.
    limit: usize,
    weighed: bool,
    out: Vec<Bytes>,
}

impl Heads<'_> {
    /// Spend `n` units, unless that passes the limit.
    fn afford(&mut self, n: usize) -> bool {
        if self.spent + n > self.limit && !self.weighed {
            self.weighed = true;
            self.limit += 2 * self.root.iter().map(node_weight).sum::<usize>();
        }
        if self.spent + n > self.limit {
            return false;
        }
        self.spent += n;
        true
    }

    /// An empty head covers every other one: nothing more to read.
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
                    None => break acc.len(), // all literal
                }
            };
            match node {
                // An escaped `\` on Windows: the expansion matches nothing
                // (§4.2), so it has no head to seed.
                Node::Literal(bytes) if has_sep(bytes) => {
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
        self.out.push(Bytes::from(head));
        acc.truncate(mark);
    }
}

/// Drop prefixes a shorter one already covers; an empty one covers all.
/// Shortest first, so a prefix is only ever covered by one kept before.
fn dedupe(mut prefixes: Vec<Bytes>) -> Vec<Bytes> {
    prefixes.sort_unstable_by(len_then_bytes);
    prefixes.dedup();
    let mut kept: Vec<Bytes> = Vec::new();
    for p in prefixes {
        if p.is_empty() {
            return vec![p];
        }
        // A cover ends where `p` has a `/`; `kept` is in the same order.
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
