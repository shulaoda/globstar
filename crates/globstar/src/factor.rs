use crate::ast::Node;

pub fn factor_branches(branches: Vec<Node>) -> Node {
    let mut seqs: Vec<Vec<Node>> = branches
        .into_iter()
        .map(|n| match n {
            Node::Concat(xs) => xs,
            other => vec![other],
        })
        .collect();
    let prefix = lift_prefix(&mut seqs);
    let suffix = lift_suffix(&mut seqs);

    let mut branches: Vec<Node> = seqs
        .into_iter()
        .map(|mut seq| match seq.len() {
            0 => Node::Concat(Vec::new()), // epsilon branch
            1 => seq.pop().unwrap(),
            _ => Node::Concat(seq),
        })
        .collect();
    let inner = if branches.len() == 1 {
        branches.pop().unwrap()
    } else {
        Node::Brace(branches)
    };

    let mut out = prefix;
    match inner {
        Node::Concat(xs) => out.extend(xs),
        other => out.push(other),
    }
    out.extend(suffix);
    if out.len() == 1 {
        out.pop().unwrap()
    } else {
        Node::Concat(out)
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

fn fold_group_at_edge<'a>(mut it: impl Iterator<Item = &'a Node>) -> usize {
    match (it.next(), it.next()) {
        (None, _) => 0,
        (Some(Node::Globstar), Some(Node::Separator)) => 2,
        (Some(Node::Separator), Some(Node::Globstar)) => {
            if matches!(it.next(), Some(Node::Separator)) {
                3
            } else {
                2
            }
        }
        _ => 1,
    }
}

fn lift_prefix(seqs: &mut [Vec<Node>]) -> Vec<Node> {
    let mut lifted = Vec::new();

    loop {
        let size = fold_group_at_edge(seqs[0].iter());
        if size == 0 {
            return lifted;
        }
        let head = &seqs[0][..size];
        let same = seqs.iter().skip(1).all(|s| {
            fold_group_at_edge(s.iter()) == size
                && s[..size].iter().zip(head).all(|(x, y)| node_eq(x, y))
        });
        if !same {
            break;
        }
        lifted.extend(seqs[0].drain(..size));
        for s in seqs.iter_mut().skip(1) {
            s.drain(..size);
        }
    }

    if !seqs
        .iter()
        .all(|s| matches!(s.first(), Some(Node::Literal(_))))
    {
        return lifted;
    }
    let lits: Vec<&[u8]> = seqs
        .iter()
        .map(|s| match &s[0] {
            Node::Literal(b) => b.as_slice(),
            _ => unreachable!("checked above"),
        })
        .collect();
    let min = lits.iter().map(|l| l.len()).min().unwrap_or(0);
    let n = (0..min)
        .take_while(|&i| {
            let b = lits[0][i];
            lits.iter().skip(1).all(|l| l[i] == b)
        })
        .count();
    if n == 0 {
        return lifted;
    }
    lifted.push(Node::Literal(lits[0][..n].to_vec()));
    for s in seqs.iter_mut() {
        let Node::Literal(b) = &mut s[0] else {
            unreachable!("checked above");
        };
        b.drain(..n);
        if b.is_empty() {
            s.remove(0);
        }
    }
    lifted
}

fn lift_suffix(seqs: &mut [Vec<Node>]) -> Vec<Node> {
    let mut lifted_reverse = Vec::new();

    loop {
        let size = fold_group_at_edge(seqs[0].iter().rev());
        if size == 0 {
            break;
        }
        let len0 = seqs[0].len();
        let tail = &seqs[0][len0 - size..];
        let same = seqs.iter().skip(1).all(|s| {
            fold_group_at_edge(s.iter().rev()) == size
                && s[s.len() - size..]
                    .iter()
                    .zip(tail)
                    .all(|(x, y)| node_eq(x, y))
        });
        if !same {
            break;
        }
        lifted_reverse.extend(seqs[0].drain(len0 - size..).rev());
        for s in seqs.iter_mut().skip(1) {
            s.truncate(s.len() - size);
        }
    }

    if seqs
        .iter()
        .all(|s| matches!(s.last(), Some(Node::Literal(_))))
    {
        let lits: Vec<&[u8]> = seqs
            .iter()
            .map(|s| match s.last().unwrap() {
                Node::Literal(b) => b.as_slice(),
                _ => unreachable!("checked above"),
            })
            .collect();
        let min = lits.iter().map(|l| l.len()).min().unwrap_or(0);
        let n = (0..min)
            .take_while(|&i| {
                let b = lits[0][lits[0].len() - 1 - i];
                lits.iter().skip(1).all(|l| l[l.len() - 1 - i] == b)
            })
            .count();
        if n > 0 {
            lifted_reverse.push(Node::Literal(lits[0][lits[0].len() - n..].to_vec()));
            for s in seqs.iter_mut() {
                let last = s.len() - 1;
                let Node::Literal(b) = &mut s[last] else {
                    unreachable!("checked above");
                };
                b.truncate(b.len() - n);
                if b.is_empty() {
                    s.pop();
                }
            }
        }
    }

    lifted_reverse.reverse();
    lifted_reverse
}
