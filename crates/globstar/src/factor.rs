use crate::ast::Node;

/// One body for the union of `branches`: what they all start or end with
/// is lifted out of the brace, whole tokens first, then the bytes shared by
/// their first or last literals. Bytes are lifted only behind a lifted
/// token, so the union still starts the way its members do and keeps
/// their static prefixes: `{src/**,spec/**}` stays, `s{rc,pec}/**` would
/// not; `src/mod{1,2}/index.ts` is fine. What is left of members that
/// differ only in the literal ending one segment is merged into one
/// member with a brace there (`merge_segments`).
#[inline(never)]
pub(crate) fn factor_branches(branches: Vec<Node>) -> Node {
    let mut seqs: Vec<Vec<Node>> = branches.into_iter().map(Node::into_seq).collect();
    let mut out = lift_tokens(&mut seqs, true);
    let bytes = !out.is_empty();
    let head = bytes.then(|| lift_bytes(&mut seqs, true)).flatten();
    let mut suffix = lift_tokens(&mut seqs, false);
    let tail = bytes.then(|| lift_bytes(&mut seqs, false)).flatten();
    let lifted_wild = out.iter().chain(&suffix).any(Node::is_wildcard);
    merge_segments(&mut seqs, lifted_wild);

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

/// Members that differ only in the literal ending one segment become
/// one member with a brace there: `**/a/**` and `**/b/**` are
/// `**/{a,b}/**`, `**/*.log` and `**/*.tmp` are `**/*{.log,.tmp}`,
/// `src/**/*.ts` and `tests/**/*.ts` are `{src,tests}/**/*.ts`. N chains
/// each comparing a name become one comparing a set. Members with a
/// wildcard take part (in what was lifted, `lifted_wild`, or in what is
/// left: a literal member sits in the literal trie already) and with
/// nothing `node_eq` cannot compare. Members alike so far are grouped by
/// segment and a cheap key, then by a hash of everything else, then
/// compared node by node. Segments are taken from the last back: a
/// member joins the deepest group it fits, and what that makes of it is
/// not keyed again at the shallower segments, where roots would collide
/// in vain.
fn merge_segments(seqs: &mut Vec<Vec<Node>>, lifted_wild: bool) {
    // (segment, cheap key, member, node) per literal ending a segment.
    let mut entries: Vec<(usize, u64, usize, usize)> = Vec::new();
    for (i, s) in seqs.iter().enumerate() {
        let mut wild = lifted_wild;
        let plain = s.iter().all(|n| match n {
            Node::Globstar | Node::Star | Node::AnyChar => {
                wild = true;
                true
            }
            Node::Separator | Node::Literal(_) => true,
            Node::Class(_) | Node::Brace(_) | Node::Concat(_) => false,
        });
        // A lone node merges with nothing: a literal is a brace
        // alternative as it is, a wildcard ends in no literal.
        if !(plain && wild && s.len() > 1) {
            continue;
        }
        let (mut k, mut start) = (0, 0);
        for end in 0..=s.len() {
            if end < s.len() && !matches!(s[end], Node::Separator) {
                continue;
            }
            if end > start && matches!(s[end - 1], Node::Literal(_)) {
                entries.push((k, cheap_key(s, end - 1), i, end - 1));
            }
            k += 1;
            start = end + 1;
        }
    }
    if entries.len() < 2 {
        return;
    }
    entries.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    // Per member once a group forms: 0 free, 1 heads a group, 2 absorbed.
    let mut state: Vec<u8> = Vec::new();
    let mut run = 0;
    while run < entries.len() {
        let (k, key, ..) = entries[run];
        let end = run
            + entries[run..]
                .iter()
                .take_while(|e| (e.0, e.1) == (k, key))
                .count();
        if end - run >= 2 {
            // Members alike so far and still free (a group's members
            // hold a brace or a placeholder by now), keyed by all they
            // hold but the literal.
            let mut keyed: Vec<(u64, usize, usize)> = entries[run..end]
                .iter()
                .filter(|&&(_, _, i, _)| state.get(i).is_none_or(|&st| st == 0))
                .map(|&(_, _, i, at)| (hash_without(&seqs[i], at), i, at))
                .collect();
            keyed.sort_unstable();
            merge_runs(seqs, &keyed, &mut state);
        }
        run = end;
    }
    if !state.is_empty() {
        let mut st = state.iter();
        seqs.retain(|_| *st.next().unwrap() != 2);
    }
}

/// Merge each run of `keyed` (hash, member, node) whose members are the
/// same node for node but for that literal, into the first of them.
fn merge_runs(seqs: &mut [Vec<Node>], keyed: &[(u64, usize, usize)], state: &mut Vec<u8>) {
    let mut run = 0;
    while run < keyed.len() {
        let hash = keyed[run].0;
        let end = run + keyed[run..].iter().take_while(|e| e.0 == hash).count();
        let mut head: Option<(usize, usize)> = None;
        let mut lits = Vec::new();
        for &(_, i, at) in &keyed[run..end] {
            match head {
                None => head = Some((i, at)),
                Some((h, h_at)) if same_but(&seqs[h], &seqs[i], h_at) => {
                    if state.is_empty() {
                        *state = vec![0; seqs.len()];
                    }
                    state[i] = 2;
                    lits.push(std::mem::replace(&mut seqs[i][at], Node::Separator));
                }
                Some(_) => {} // a hash collision stays as it is
            }
        }
        if let (Some((h, at)), false) = (head, lits.is_empty()) {
            state[h] = 1;
            lits.insert(0, std::mem::replace(&mut seqs[h][at], Node::Separator));
            seqs[h][at] = Node::Brace(lits);
        }
        run = end;
    }
}

/// A cheap key: the node count and the first node (or the last, when
/// the first is the literal `at` itself) by kind, length and edge
/// bytes; enough to tell most members apart before `hash_without` has
/// to.
fn cheap_key(s: &[Node], at: usize) -> u64 {
    let edge = |b: &[u8]| (b.len() as u64) << 16 | (b[0] as u64) << 8 | *b.last().unwrap() as u64;
    let node = match &s[if at == 0 { s.len() - 1 } else { 0 }] {
        Node::Literal(b) if !b.is_empty() => 2 << 32 | edge(b),
        Node::Literal(_) => 2 << 32,
        Node::Separator => 3 << 32,
        Node::Globstar => 4 << 32,
        Node::Star => 5 << 32,
        Node::AnyChar => 6 << 32,
        Node::Class(_) | Node::Brace(_) | Node::Concat(_) => unreachable!("not a candidate"),
    };
    (s.len() as u64) << 40 ^ node
}

/// `s` hashed for equality (FNV-1a), with a marker in place of node `skip`.
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

/// Are `a` and `b` the same node for node, but for a literal at `at`?
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
