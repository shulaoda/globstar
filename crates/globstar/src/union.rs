//! A union's members factored into one body.

use crate::parse::ast::Node;

/// What merging may read: this many nodes per node of the members, at
/// least `MIN_MERGE_READS`.
const MERGE_READS_PER_NODE: usize = 16;
const MIN_MERGE_READS: usize = 4096;

/// A member's part in a merge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Free,
    /// Its trailing literal became the brace.
    Head,
    /// Joined a head's brace: dropped at the end.
    Absorbed,
}

/// The tokens all members start or end with are lifted out of the brace,
/// then the bytes their first or last literals share (only behind a
/// lifted token, so the static prefixes are kept), and members that
/// differ only in the literal ending one segment are merged into one
/// with a brace there. Members left within one segment share a brace
/// only when it is a set (literal branches) or holds no literal at all;
/// a mix would be one in-segment NFA, so the tokens go back and the
/// members stay apart.
#[inline(never)]
pub(crate) fn factor_branches(branches: Vec<Node>) -> Node {
    let mut seqs: Vec<Vec<Node>> = branches.into_iter().map(Node::into_seq).collect();
    let mut out = lift_tokens(&mut seqs, true);
    let mut suffix = lift_tokens(&mut seqs, false);
    let (mut single, mut literals, mut some_literal) = (0, true, false);
    'members: for s in &seqs {
        let mut has_literal = false;
        for n in s {
            match n {
                Node::Separator => continue 'members,
                Node::Literal(_) => has_literal = true,
                _ => {}
            }
        }
        single += 1;
        literals &= s.len() <= 1 && (s.is_empty() || has_literal);
        some_literal |= has_literal;
    }
    let a_set = literals
        && suffix
            .iter()
            .take_while(|n| !matches!(n, Node::Separator))
            .all(|n| matches!(n, Node::Literal(_)));
    if single > 1 && !a_set && some_literal && !(out.is_empty() && suffix.is_empty()) {
        for s in &mut seqs {
            let mut whole = out.clone();
            whole.append(s);
            whole.extend(suffix.iter().cloned());
            *s = whole;
        }
        out.clear();
        suffix.clear();
    }
    let bytes = !out.is_empty();
    let head = bytes.then(|| lift_bytes(&mut seqs, true)).flatten();
    let tail = bytes.then(|| lift_bytes(&mut seqs, false)).flatten();

    merge_segments(
        &mut seqs,
        out.iter()
            .chain(&suffix)
            .any(|n| matches!(n, Node::Star | Node::AnyChar | Node::Globstar)),
    );

    // Members that differ only in spelling leave nothing behind: one
    // empty branch stands for them all.
    let mut empty = false;
    let mut branches: Vec<Node> = seqs
        .into_iter()
        .filter_map(|seq| {
            if seq.is_empty() {
                if empty {
                    return None;
                }
                empty = true;
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

/// Members that differ only in the literal ending one segment become one
/// member with a brace there: `**/a/**` and `**/b/**` are `**/{a,b}/**`.
/// Members with a wildcard take part (in the member or lifted). They are
/// grouped by segment and a hash of everything but that literal, then
/// compared node by node, from the last segment back; a member joins the
/// deepest group it fits. Out of line: inlined into `factor_branches` it
/// builds unions up to 0.4% slower.
#[inline(never)]
fn merge_segments(seqs: &mut Vec<Vec<Node>>, lifted_wild: bool) {
    // (segment, hash, member, node) per literal ending a segment.
    let mut entries: Vec<(usize, u64, usize, usize)> = Vec::new();
    let (mut reads, mut nodes) = (0, 0);
    for (i, s) in seqs.iter().enumerate() {
        nodes += s.len();
        let mut wild = lifted_wild;
        let plain = s.iter().all(|n| match n {
            Node::Globstar | Node::Star | Node::AnyChar => {
                wild = true;
                true
            }
            Node::Separator | Node::Literal(_) => true,
            Node::Class(_) | Node::Brace(_) | Node::Concat(_) => false,
        });
        if !(plain && wild && s.len() > 1) {
            continue;
        }
        let mut k = 0;
        for at in 0..s.len() {
            match s[at] {
                Node::Separator => k += 1,
                Node::Literal(_) if s.get(at + 1).is_none_or(|n| matches!(n, Node::Separator)) => {
                    entries.push((k, 0, i, at));
                    reads += s.len();
                }
                _ => {}
            }
        }
    }
    if entries.len() < 2 || reads > (MERGE_READS_PER_NODE * nodes).max(MIN_MERGE_READS) {
        return;
    }
    for e in &mut entries {
        e.1 = hash_without(&seqs[e.2], e.3);
    }
    entries.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut state = vec![Member::Free; seqs.len()];
    for alike in entries.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)) {
        let Some(&(_, _, h, h_at)) = alike.iter().find(|e| state[e.2] == Member::Free) else {
            continue;
        };
        let mut lits = Vec::new();
        for &(_, _, i, at) in alike {
            if i != h && state[i] == Member::Free && same_but(&seqs[h], &seqs[i], h_at) {
                state[i] = Member::Absorbed;
                lits.push(std::mem::replace(&mut seqs[i][at], Node::Separator));
            }
        }
        if !lits.is_empty() {
            state[h] = Member::Head;
            lits.insert(0, std::mem::replace(&mut seqs[h][h_at], Node::Separator));
            seqs[h][h_at] = Node::Brace(lits);
        }
    }
    let mut st = state.iter();
    seqs.retain(|_| *st.next().unwrap() != Member::Absorbed);
}

/// FNV-1a of `s`, with a marker in place of node `skip`.
fn hash_without(s: &[Node], skip: usize) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    };
    for (i, node) in s.iter().enumerate() {
        match node {
            _ if i == skip => mix(1),
            Node::Literal(b) => {
                mix(2);
                (b.len() as u32)
                    .to_le_bytes()
                    .iter()
                    .chain(b)
                    .for_each(|&x| mix(x));
            }
            Node::Separator => mix(3),
            Node::Globstar => mix(4),
            Node::Star => mix(5),
            Node::AnyChar => mix(6),
            Node::Class(_) | Node::Brace(_) | Node::Concat(_) => unreachable!("not a candidate"),
        }
    }
    hash
}

fn same_but(a: &[Node], b: &[Node], at: usize) -> bool {
    a.len() == b.len()
        && matches!(b[at], Node::Literal(_))
        && a.iter()
            .zip(b)
            .enumerate()
            .all(|(j, (x, y))| j == at || node_eq(x, y))
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

/// The whole tokens every member starts (or ends) with, taken out of them.
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
