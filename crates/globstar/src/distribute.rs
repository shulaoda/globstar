//! Braces taken apart around a `**` (GLOB_SPEC §7.0, §7.7).
//!
//! A `**` beside a brace meets a different token in every branch, and a
//! `**` at the edge of a branch may meet a brace outside. [`distribute`]
//! rewrites such a sequence until no `**` is left facing a brace edge that
//! may be a separator, or empty, in some expansion: `**{/a,b}` becomes
//! `{**/a,**b}`, `{**,a}{/,b}` becomes `{**/,**b,a{/,b}}`. Afterwards the
//! tokens beside each `**` tell what it meets in every expansion, and
//! [`crate::resolve`] decides it.
//!
//! The work is linear in the output: every sequence caches the edges of
//! its expansions, which no merge changes. What a merge copies is charged
//! to a budget (§7.7), and the braces it nests are held to the nesting
//! limit (§7.6).

use crate::ast::Node;
use crate::error::{GlobError, MAX_BRACE_NESTING, MAX_EXPANSION};

/// `budget` is what the rewrite may still copy.
pub(crate) fn distribute(node: Node, budget: &mut usize) -> Result<Node, GlobError> {
    Ok(node_of(rewrite(node, budget)?))
}

/// What an expansion can start or end with, as a bit set.
type Edge = u8;
/// A separator.
const SEP: Edge = 1;
/// A `**` candidate.
const GLOB: Edge = 2;
/// Any other token.
const TEXT: Edge = 4;
/// Nothing: the expansion is empty, so the edge lies beyond.
const NONE: Edge = 8;

/// A sequence under distribution. It knows the `[first, last]` edges of
/// its expansions, which no merge changes, so no merge has to look inside.
/// `depth` bounds how deep braces nest in it; it never shrinks, so it can
/// run a level or two ahead after a merge flattens a brace.
#[derive(Clone)]
struct Seq {
    items: Vec<Item>,
    edges: [Edge; 2],
    depth: usize,
}

#[derive(Clone)]
enum Item {
    Token(Node),
    Brace {
        branches: Vec<Seq>,
        edges: [Edge; 2],
        depth: usize,
    },
}

impl Item {
    fn edges(&self) -> [Edge; 2] {
        match self {
            Item::Brace { edges, .. } => *edges,
            Item::Token(Node::Separator) => [SEP; 2],
            Item::Token(Node::Globstar) => [GLOB; 2],
            Item::Token(_) => [TEXT; 2],
        }
    }

    fn depth(&self) -> usize {
        match self {
            Item::Brace { depth, .. } => *depth,
            Item::Token(_) => 0,
        }
    }
}

/// The edges of `a` followed by `b`.
fn join(a: [Edge; 2], b: [Edge; 2]) -> [Edge; 2] {
    let through = |edge: Edge, beyond: Edge| match edge & NONE {
        0 => edge,
        _ => edge & !NONE | beyond,
    };
    [through(a[0], b[0]), through(b[1], a[1])]
}

fn seq_of(items: Vec<Item>) -> Seq {
    let edges = items
        .iter()
        .fold([NONE; 2], |e, item| join(e, item.edges()));
    let depth = items.iter().map(Item::depth).max().unwrap_or(0);
    Seq {
        items,
        edges,
        depth,
    }
}

/// A brace of `branches`; a branch that is one brace gives its branches.
fn brace_of(branches: Vec<Seq>) -> Item {
    let mut flat = Vec::with_capacity(branches.len());
    for mut branch in branches {
        match branch.items.as_mut_slice() {
            [Item::Brace { branches, .. }] => flat.append(branches),
            _ => flat.push(branch),
        }
    }
    let edges = flat.iter().fold([0; 2], |e, branch| {
        [e[0] | branch.edges[0], e[1] | branch.edges[1]]
    });
    let depth = 1 + flat.iter().map(|branch| branch.depth).max().unwrap_or(0);
    Item::Brace {
        branches: flat,
        edges,
        depth,
    }
}

/// One sequence for all of `seqs`: the only one, or a brace of them.
fn group(mut seqs: Vec<Seq>) -> Seq {
    match seqs.len() {
        1 => seqs.pop().unwrap(),
        _ => seq_of(vec![brace_of(seqs)]),
    }
}

/// The sequence `node` stands for, with every junction merged.
fn rewrite(node: Node, budget: &mut usize) -> Result<Seq, GlobError> {
    let nodes = match node {
        Node::Concat(nodes) => nodes,
        node => vec![node],
    };
    let mut out = seq_of(Vec::with_capacity(nodes.len()));
    let mut nodes = nodes.into_iter().peekable();
    while let Some(node) = nodes.next() {
        let item = match node {
            Node::Brace(branches) => {
                let branches = branches.into_iter().map(|b| rewrite(b, budget));
                brace_of(branches.collect::<Result<_, _>>()?)
            }
            // Beside what is never a separator, a `**` is a star in every
            // expansion and has nothing to meet.
            Node::Globstar
                if out.edges[1] & (SEP | NONE) == 0
                    || nodes
                        .peek()
                        .is_some_and(|n| !matches!(n, Node::Separator | Node::Brace(_))) =>
            {
                Item::Token(Node::Star)
            }
            token => Item::Token(token),
        };
        out.edges = join(out.edges, item.edges());
        push(&mut out, item, budget)?;
    }
    Ok(out)
}

/// Pushes `item` and merges the junction it closes; pushing the merged
/// items can close another one with the items before them. Braces may nest
/// no deeper than in a pattern (§7.6).
fn push(seq: &mut Seq, item: Item, budget: &mut usize) -> Result<(), GlobError> {
    if item.depth() > MAX_BRACE_NESTING {
        return Err(GlobError::BraceNestingTooDeep {
            max: MAX_BRACE_NESTING,
        });
    }
    seq.depth = seq.depth.max(item.depth());
    seq.items.push(item);
    if junction(&seq.items) {
        let b = seq.items.pop().unwrap();
        let a = seq.items.pop().unwrap();
        for item in merge(a, b, budget)? {
            push(seq, item, budget)?;
        }
    }
    Ok(())
}

/// Do the last two items form a junction: a brace or a `**` beside a brace,
/// meeting at a `**` edge?
fn junction(items: &[Item]) -> bool {
    let joins = |item: &Item| matches!(item, Item::Brace { .. } | Item::Token(Node::Globstar));
    matches!(items, [.., a, b] if joins(a) && joins(b) && meets(a.edges()[1], b.edges()[0]))
}

/// Does a `**` on one edge face a possible separator, or nothing, on the
/// other? Otherwise every `**` here is a star in all expansions.
fn meets(a: Edge, b: Edge) -> bool {
    let faces = |glob: Edge, edge: Edge| glob & GLOB != 0 && edge & (SEP | NONE) != 0;
    faces(a, b) || faces(b, a)
}

/// Merges a junction: one of its braces opens, and each group of its
/// branches takes in a copy of the other item.
fn merge(a: Item, b: Item, budget: &mut usize) -> Result<Vec<Item>, GlobError> {
    // The first brace opens. The last one does instead after a `**` token,
    // or when every branch of it starts with a `/` token: they share that
    // `/`, so nothing is copied.
    let front = match (&a, &b) {
        (Item::Token(_), _) => true,
        (_, Item::Brace { branches, .. }) => branches
            .iter()
            .all(|b| matches!(b.items.first(), Some(Item::Token(Node::Separator)))),
        _ => false,
    };
    let (opened, other) = if front { (b, a) } else { (a, b) };
    let Item::Brace { branches, .. } = opened else {
        unreachable!("a junction holds a brace")
    };
    let other = seq_of(vec![other]);
    let groups = split(branches, other.edges[usize::from(front)], front);

    if groups.len() > 1 {
        let copied = (groups.len() - 1).saturating_mul(weight(&other.items));
        *budget = budget
            .checked_sub(copied)
            .ok_or(GlobError::BraceExpansionTooLarge { max: MAX_EXPANSION })?;
    }
    let mut merged = Vec::with_capacity(groups.len());
    let others = std::iter::repeat_n(other, groups.len());
    for (branch, other) in groups.into_iter().zip(others) {
        merged.push(attach(branch, other, front, budget)?);
    }
    Ok(group(merged).items)
}

/// Sorts the branches of an opened brace into groups; each group then takes
/// one copy of the other item, whose edge beside them is `facing`. The
/// branches that edge does not meet stay one brace. So do the branches with
/// a `/` token beside it: they share that one `/`. Every other branch is a
/// group of its own.
fn split(branches: Vec<Seq>, facing: Edge, front: bool) -> Vec<Seq> {
    let (mut rest, mut seps, mut groups) = (Vec::new(), Vec::new(), Vec::new());
    for mut branch in branches {
        let end = if front {
            branch.items.first()
        } else {
            branch.items.last()
        };
        if !meets(facing, branch.edges[usize::from(!front)]) {
            rest.push(branch);
        } else if matches!(end, Some(Item::Token(Node::Separator))) {
            if front {
                branch.items.remove(0);
            } else {
                branch.items.pop();
            }
            seps.push(seq_of(branch.items));
        } else {
            groups.push(branch);
        }
    }
    if !seps.is_empty() {
        let mut items = group(seps).items;
        let at = if front { 0 } else { items.len() };
        items.insert(at, Item::Token(Node::Separator));
        groups.push(seq_of(items));
    }
    if !rest.is_empty() {
        groups.push(group(rest));
    }
    groups
}

/// `seq` with `other` behind it, or in front of it, merging the junctions
/// that form at the seam.
fn attach(seq: Seq, other: Seq, front: bool, budget: &mut usize) -> Result<Seq, GlobError> {
    let (mut head, tail) = if front { (other, seq) } else { (seq, other) };
    head.edges = join(head.edges, tail.edges);
    for item in tail.items {
        push(&mut head, item, budget)?;
    }
    Ok(head)
}

/// What a copy of `items` costs: one per token, a literal or a class by
/// its length, and one per branch.
fn weight(items: &[Item]) -> usize {
    let cost = |item: &Item| match item {
        Item::Token(Node::Literal(bytes)) => bytes.len(),
        Item::Token(Node::Class(class)) => 1 + class.items.len(),
        Item::Token(_) => 1,
        Item::Brace { branches, .. } => branches.iter().map(|b| 1 + weight(&b.items)).sum(),
    };
    items.iter().map(cost).sum()
}

/// Back to a node.
fn node_of(seq: Seq) -> Node {
    let node = |item| match item {
        Item::Token(node) => node,
        Item::Brace { branches, .. } => Node::Brace(branches.into_iter().map(node_of).collect()),
    };
    let mut nodes: Vec<Node> = seq.items.into_iter().map(node).collect();
    match nodes.len() {
        1 => nodes.pop().unwrap(),
        _ => Node::Concat(nodes),
    }
}
