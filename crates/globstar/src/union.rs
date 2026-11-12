use crate::parse::ast::Node;

/// A literal ending segment `k` of member `i`, at node `at`, with the
/// member's hash masked there: members alike but for that literal share it.
/// The hash is the JS twin's, bit for bit, so a collision groups the same
/// members in both.
struct Entry {
    k: usize,
    hash: u32,
    i: usize,
    at: usize,
}

#[inline(never)]
pub(crate) fn factor_branches(branches: Vec<Node>) -> Node {
    let mut seqs: Vec<Vec<Node>> = branches.into_iter().map(Node::into_seq).collect();
    // Members apart in the literal ending one segment become one member
    // with a brace of those literals there.
    let mut entries = Vec::with_capacity(seqs.len());
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
                    let hash = whole.wrapping_sub(salted(lit_hash(lit), at));
                    entries.push(Entry { k, hash, i, at });
                }
                _ => {}
            }
        }
    }
    // Deepest segment first: a member takes part in one merge at most,
    // the deepest it fits, as the head or as one absorbed. One compared
    // with a head it does not fit, which only a hash collision groups,
    // takes part in none.
    entries.sort_unstable_by(|a, b| b.k.cmp(&a.k).then(a.hash.cmp(&b.hash)).then(a.i.cmp(&b.i)));
    let mut done = vec![false; seqs.len()];
    let mut absorbed = vec![false; seqs.len()];
    for alike in entries.chunk_by(|a, b| (a.k, a.hash) == (b.k, b.hash)) {
        let Some(head) = alike.iter().find(|e| !done[e.i]) else {
            continue;
        };
        let (h, at) = (head.i, head.at);
        let mut lits = Vec::with_capacity(alike.len());
        for e in alike {
            if e.i == h || done[e.i] {
                continue;
            }
            done[e.i] = true;
            if same_but(&seqs[h], &seqs[e.i], at) {
                absorbed[e.i] = true;
                lits.push(std::mem::replace(&mut seqs[e.i][at], Node::Separator));
            }
        }
        if !lits.is_empty() {
            done[h] = true;
            lits.insert(0, std::mem::replace(&mut seqs[h][at], Node::Separator));
            seqs[h][at] = Node::Brace(lits);
        }
    }
    let mut gone = absorbed.iter();
    seqs.retain(|_| !*gone.next().unwrap());
    let mut members: Vec<Node> = seqs.into_iter().map(Node::of_seq).collect();
    if members.len() == 1 {
        members.pop().unwrap()
    } else {
        Node::Brace(members)
    }
}

/// The hash of a member made of literals, separators and the wildcards
/// `*`, `?` and `**`, with a wildcard among them: the sum of its node
/// hashes, each mixed with its position.
fn member_hash(s: &[Node]) -> Option<u32> {
    let mut hash = 0u32;
    let mut wild = false;
    for (at, n) in s.iter().enumerate() {
        let h = match n {
            Node::Literal(lit) => lit_hash(lit),
            Node::Separator => 1,
            Node::Globstar => {
                wild = true;
                2
            }
            Node::Star => {
                wild = true;
                3
            }
            Node::AnyChar => {
                wild = true;
                4
            }
            Node::Class(_) | Node::Brace(_) | Node::Concat(_) => return None,
        };
        hash = hash.wrapping_add(salted(h, at));
    }
    wild.then_some(hash)
}

fn lit_hash(lit: &[u8]) -> u32 {
    lit.iter().fold(0x811c_9dc5 ^ lit.len() as u32, |h, &x| {
        (h ^ x as u32).wrapping_mul(0x0100_0193)
    })
}

/// A node's hash mixed with its position: the sum over a member changes by
/// one term when one node does.
fn salted(hash: u32, at: usize) -> u32 {
    let x = hash ^ (at as u32 + 1).wrapping_mul(0x9e37_79b1);
    (x ^ (x >> 15)).wrapping_mul(0x85eb_ca6b)
}

/// `b` is `a` but for a literal at `at`.
fn same_but(a: &[Node], b: &[Node], at: usize) -> bool {
    a.len() == b.len()
        && matches!(b[at], Node::Literal(_))
        && a.iter()
            .zip(b)
            .enumerate()
            .all(|(j, (x, y))| j == at || node_eq(x, y))
}

/// Equal nodes of plain members: a literal's bytes, else the kind.
fn node_eq(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::Literal(x), Node::Literal(y)) => x == y,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}
