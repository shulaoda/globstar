use crate::ast::Node;

/// One body for the union of `branches`: what they all start or end with
/// is lifted out of the brace, whole tokens first, then the bytes shared by
/// their first or last literals. Bytes are lifted only behind a lifted
/// token, so the union still starts the way its members do and keeps
/// their static prefixes: `{src/**,spec/**}` stays, `s{rc,pec}/**` would
/// not; `src/mod{1,2}/index.ts` is fine.
#[inline(never)]
pub(crate) fn factor_branches(branches: Vec<Node>) -> Node {
    let mut seqs: Vec<Vec<Node>> = branches.into_iter().map(Node::into_seq).collect();
    let mut out = lift_tokens(&mut seqs, true);
    let bytes = !out.is_empty();
    let head = bytes.then(|| lift_bytes(&mut seqs, true)).flatten();
    let mut suffix = lift_tokens(&mut seqs, false);
    let tail = bytes.then(|| lift_bytes(&mut seqs, false)).flatten();

    // Members that differ only in spelling (`**` and `**/**`, `\a` and
    // `a`) leave nothing behind; one empty branch stands for them all,
    // so a union of equal members is that member again.
    let mut empty = false;
    let mut branches: Vec<Node> = seqs
        .into_iter()
        .filter_map(|seq| {
            if seq.is_empty() {
                if empty {
                    return None;
                }
                empty = true; // the epsilon branch
            }
            Some(Node::of_seq(seq))
        })
        .collect();
    let inner = if branches.len() == 1 {
        branches.pop().unwrap()
    } else {
        Node::Brace(branches)
    };

    out.extend(head);
    match inner {
        Node::Concat(xs) => out.extend(xs),
        other => out.push(other),
    }
    out.extend(tail);
    out.append(&mut suffix);
    Node::of_seq(out)
}

/// The `k`th node of `seq` from the front, or from the back.
fn at(seq: &[Node], k: usize, front: bool) -> Option<&Node> {
    if front {
        seq.get(k)
    } else {
        seq.len().checked_sub(k + 1).map(|i| &seq[i])
    }
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

/// How many nodes from the `k`th on stay together: `**/`, `/**/` and
/// `/**` are lifted whole or not at all.
fn group(seq: &[Node], k: usize, front: bool) -> usize {
    match (at(seq, k, front), at(seq, k + 1, front)) {
        (None, _) => 0,
        (Some(Node::Globstar), Some(Node::Separator)) => 2,
        (Some(Node::Separator), Some(Node::Globstar)) => {
            if matches!(at(seq, k + 2, front), Some(Node::Separator)) {
                3
            } else {
                2
            }
        }
        _ => 1,
    }
}

/// The whole tokens every member starts (or ends) with, taken out of
/// them: counted first, then drained once per member.
fn lift_tokens(seqs: &mut [Vec<Node>], front: bool) -> Vec<Node> {
    let mut n = 0;
    loop {
        let first = &seqs[0];
        let size = group(first, n, front);
        let same = |s: &Vec<Node>| {
            group(s, n, front) == size
                && (n..n + size)
                    .all(|k| node_eq(at(s, k, front).unwrap(), at(first, k, front).unwrap()))
        };
        if size == 0 || !seqs[1..].iter().all(same) {
            break;
        }
        n += size;
    }
    let mut lifted = Vec::new();
    for (i, s) in seqs.iter_mut().enumerate() {
        let len = s.len();
        let cut = if front {
            s.drain(..n)
        } else {
            s.drain(len - n..)
        };
        if i == 0 {
            lifted.extend(cut);
        }
    }
    lifted
}

/// The bytes every member's first (or last) literal shares, taken out of
/// them as one literal.
fn lift_bytes(seqs: &mut [Vec<Node>], front: bool) -> Option<Node> {
    let edge_lit = |s| match at(s, 0, front) {
        Some(Node::Literal(b)) => Some(b.as_slice()),
        _ => None,
    };
    let lits: Vec<&[u8]> = seqs.iter().map(|s| edge_lit(s)).collect::<Option<_>>()?;
    let byte = |l: &[u8], i: usize| if front { l[i] } else { l[l.len() - 1 - i] };
    let min = lits.iter().map(|l| l.len()).min().unwrap_or(0);
    let n = (0..min)
        .take_while(|&i| lits.iter().all(|l| byte(l, i) == byte(lits[0], i)))
        .count();
    if n == 0 {
        return None;
    }
    let shared = if front {
        lits[0][..n].to_vec()
    } else {
        lits[0][lits[0].len() - n..].to_vec()
    };
    for s in seqs.iter_mut() {
        let k = if front { 0 } else { s.len() - 1 };
        let Node::Literal(b) = &mut s[k] else {
            unreachable!("every edge is a literal")
        };
        if front {
            b.drain(..n);
        } else {
            b.truncate(b.len() - n);
        }
        if b.is_empty() {
            s.remove(k);
        }
    }
    Some(Node::Literal(shared))
}
