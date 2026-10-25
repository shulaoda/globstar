use crate::parse::ast::Node;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Free,
    Head,
    Absorbed,
}

#[inline(never)]
pub(crate) fn factor_branches(branches: Vec<Node>) -> Node {
    let mut seqs: Vec<Vec<Node>> = branches.into_iter().map(Node::into_seq).collect();
    merge_segments(&mut seqs);
    let mut branches: Vec<Node> = seqs.into_iter().map(Node::of_seq).collect();
    if branches.len() == 1 {
        branches.pop().unwrap()
    } else {
        Node::Brace(branches)
    }
}

/// Members apart in the literal ending one segment become one member with a
/// brace of those literals there.
#[inline(never)]
fn merge_segments(seqs: &mut Vec<Vec<Node>>) {
    // (segment, hash of the member with that literal masked, member, node)
    // per literal ending a segment of a plain member with a wildcard.
    let mut entries: Vec<(usize, u64, usize, usize)> = Vec::with_capacity(seqs.len());
    for (i, s) in seqs.iter().enumerate() {
        let Some(whole) = member_hash(s) else {
            continue;
        };
        let mut k = 0;
        for (at, n) in s.iter().enumerate() {
            match n {
                Node::Separator => k += 1,
                Node::Literal(lit)
                    if s.get(at + 1).is_none_or(|n| matches!(n, Node::Separator)) =>
                {
                    let masked = whole
                        .wrapping_sub(salted(lit_hash(lit), at))
                        .wrapping_add(salted(MASK, at));
                    entries.push((k, masked, i, at));
                }
                _ => {}
            }
        }
    }
    if entries.len() < 2 {
        return;
    }
    entries.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut state = vec![Part::Free; seqs.len()];
    for alike in entries.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)) {
        let Some(&(_, _, h, h_at)) = alike.iter().find(|e| state[e.2] == Part::Free) else {
            continue;
        };
        let mut lits = Vec::with_capacity(alike.len());
        for &(_, _, i, at) in alike {
            if i != h && state[i] == Part::Free && same_but(&seqs[h], &seqs[i], h_at) {
                state[i] = Part::Absorbed;
                lits.push(std::mem::replace(&mut seqs[i][at], Node::Separator));
            }
        }
        if !lits.is_empty() {
            state[h] = Part::Head;
            lits.insert(0, std::mem::replace(&mut seqs[h][h_at], Node::Separator));
            seqs[h][h_at] = Node::Brace(lits);
        }
    }
    let mut st = state.iter();
    seqs.retain(|_| *st.next().unwrap() != Part::Absorbed);
}

const MASK: u64 = 1;

/// The hash of a member made of literals, separators and the wildcards
/// `*`, `?` and `**`, with a wildcard among them and more than one node:
/// the sum of its node hashes, each mixed with its position.
fn member_hash(s: &[Node]) -> Option<u64> {
    let mut wild = false;
    let mut hash = s.len() as u64;
    for (at, n) in s.iter().enumerate() {
        let h = match n {
            Node::Literal(lit) => lit_hash(lit),
            Node::Separator => 3,
            Node::Globstar => {
                wild = true;
                4
            }
            Node::Star => {
                wild = true;
                5
            }
            Node::AnyChar => {
                wild = true;
                6
            }
            Node::Class(_) | Node::Brace(_) | Node::Concat(_) => return None,
        };
        hash = hash.wrapping_add(salted(h, at));
    }
    (wild && s.len() > 1).then_some(hash)
}

fn lit_hash(lit: &[u8]) -> u64 {
    lit.iter()
        .fold(0xcbf2_9ce4_8422_2325 ^ lit.len() as u64, |h, &x| {
            (h ^ x as u64).wrapping_mul(0x0100_0000_01b3)
        })
}

/// A node's hash mixed with its position: the sum over a member changes by
/// one term when one node does.
fn salted(hash: u64, at: usize) -> u64 {
    let x = hash ^ (at as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (x ^ (x >> 31)).wrapping_mul(0xBF58_476D_1CE4_E5B9)
}

fn same_but(a: &[Node], b: &[Node], at: usize) -> bool {
    a.len() == b.len()
        && matches!(b[at], Node::Literal(_))
        && a.iter()
            .zip(b)
            .enumerate()
            .all(|(j, (x, y))| j == at || node_eq(x, y))
}

fn node_eq(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::Literal(x), Node::Literal(y)) => x == y,
        (Node::Separator, Node::Separator)
        | (Node::Globstar, Node::Globstar)
        | (Node::AnyChar, Node::AnyChar)
        | (Node::Star, Node::Star) => true,
        _ => false,
    }
}
