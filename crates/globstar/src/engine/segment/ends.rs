//! The literal ends every match shares, read off the AST as written:
//! the suffixes the prefilter checks and the static prefixes a walker
//! seeds from (theory 03, §13 and §14).

use crate::engine::bytes::has_escaped_sep;
use crate::parse::ast::{Node, ends_in_globstar};

use super::Bytes;
use super::graph::{node_weight, room};

/// A pattern that ends in a brace of more branches gets one suffix: the
/// one they all share.
const MAX_SUFFIX_SET: usize = 16;

/// How deep braces may nest along one head.
const MAX_HEAD_DEPTH: usize = 64;

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

/// One suffix per branch of a trailing brace, or past `MAX_SUFFIX_SET`
/// branches the suffix the branches' own tails share; none when a branch
/// guarantees nothing.
fn suffix_set(nodes: &[Node]) -> Box<[Bytes]> {
    let Some(Node::Brace(branches)) = nodes.last() else {
        return Box::default();
    };
    let before = &nodes[..nodes.len() - 1];
    let mut common = Vec::new();
    push_run(trailing(before, false), &mut common);
    let after_globstar = ends_in_globstar(before, false);
    let many = branches.len() > MAX_SUFFIX_SET;
    let mut set = Vec::new();
    let mut shared: Option<Vec<u8>> = None;
    let mut acc = Vec::new();
    for branch in branches {
        acc.clear();
        // The common tail joins a branch only when the branch is all
        // literal; otherwise it would claim more than the branch does.
        if !many
            && branch
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
        if !many {
            set.push(Bytes::from(&acc[..]));
            continue;
        }
        shared = Some(match shared {
            None => acc.clone(),
            Some(s) => {
                let n = s
                    .iter()
                    .rev()
                    .zip(acc.iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();
                s[s.len() - n..].to_vec()
            }
        });
    }
    match shared {
        None => set.into_boxed_slice(),
        Some(s) if s.is_empty() => Box::default(),
        Some(s) => Box::new([s.into_boxed_slice()]),
    }
}

/// The deepest segment-bounded literal head of each brace expansion,
/// deduplicated. Every byte read and branch taken costs one unit of the
/// room; past it, or past `MAX_HEAD_DEPTH`, a head ends at its last whole
/// segment, a shorter prefix that still holds. Past a room of bytes
/// copied into heads, the reading starts again at the depth limit, which
/// gives the one head before the first brace.
pub(super) fn static_prefixes(body: &Node, weight: usize) -> Box<[Bytes]> {
    let read_at = |depth| {
        let mut heads = Heads {
            spent: 0,
            budget: room(weight),
            copied: 0,
            out: Vec::with_capacity(1),
        };
        heads.read(body.seq(), None, &mut Vec::new(), 0, depth);
        heads
    };
    let mut heads = read_at(0);
    if heads.copied > heads.budget {
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
                // before whole; the look at the branches is paid too.
                Node::Brace(bs)
                    if self.afford(bs.len())
                        && bs
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
        if self.copied > self.budget {
            self.out = vec![Bytes::default()];
        } else {
            self.out.push(Bytes::from(head));
        }
        acc.truncate(mark);
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
