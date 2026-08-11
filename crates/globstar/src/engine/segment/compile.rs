use crate::engine::ops::Op;

use super::seg_nfa::SegNfa;
use super::{Elem, ElemSeq, MAX_FORKS, MAX_SEQ_STATES, Wild, WildKind};

pub(super) fn compile_seqs(ops: &[Op], dot: bool, ci: bool) -> Option<Vec<ElemSeq>> {
    if !ops
        .iter()
        .any(|op| matches!(op, Op::Alternation(_)) && op_crosses_segment(op))
    {
        return Some(vec![segmentize_fork(ops, dot, ci)?]);
    }
    let op_seqs = expand_forks(ops)?;
    let mut seqs = Vec::with_capacity(op_seqs.len());
    for fork in &op_seqs {
        seqs.push(segmentize_fork(fork, dot, ci)?);
    }
    Some(seqs)
}

fn segmentize_fork(ops: &[Op], dot: bool, ci: bool) -> Option<ElemSeq> {
    let glued = ops.windows(2).any(|w| {
        matches!(w[1], Op::GlobstarAny) && matches!(w[0], Op::OptSegmentsSlash | Op::SepRun)
    });
    if !glued {
        return segmentize(ops, dot, ci);
    }
    let mut flat = ops.to_vec();
    collapse_open_globstars(&mut flat);
    segmentize(&flat, dot, ci)
}

fn collapse_open_globstars(ops: &mut Vec<Op>) {
    let mut i = 0;
    while i < ops.len() {
        if i > 0 && matches!(ops[i], Op::GlobstarAny) {
            match ops[i - 1] {
                Op::OptSegmentsSlash => {
                    ops.remove(i - 1);
                    i -= 1;
                    continue;
                }
                Op::SepRun => {
                    ops[i - 1] = Op::SlashAnything;
                    ops.remove(i);
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
}

fn expand_forks(ops: &[Op]) -> Option<Vec<Vec<Op>>> {
    let mut seqs: Vec<Vec<Op>> = vec![Vec::with_capacity(ops.len())];
    for op in ops {
        if matches!(op, Op::Alternation(_)) && op_crosses_segment(op) {
            let Op::Alternation(branches) = op else {
                unreachable!()
            };
            let mut expanded: Vec<Vec<Op>> = Vec::new();
            for branch in branches {
                let sub = expand_forks(branch)?;
                expanded.extend(sub);
                if expanded.len() > MAX_FORKS {
                    return None;
                }
            }
            let mut next = Vec::with_capacity(seqs.len().saturating_mul(expanded.len()));
            for seq in &seqs {
                for exp in &expanded {
                    if next.len() >= MAX_FORKS {
                        return None;
                    }
                    let mut merged = seq.clone();
                    merged.extend(exp.iter().cloned());
                    next.push(merged);
                }
            }
            seqs = next;
        } else {
            for seq in seqs.iter_mut() {
                seq.push(op.clone());
            }
        }
    }
    Some(seqs)
}

fn op_crosses_segment(op: &Op) -> bool {
    match op {
        Op::Sep
        | Op::SepRun
        | Op::Globstar
        | Op::OptSegmentsSlash
        | Op::SlashAnything
        | Op::GlobstarAny
        | Op::LeadingSeps => true,
        Op::Alternation(branches) => branches.iter().any(|b| b.iter().any(op_crosses_segment)),
        _ => false,
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Boundary {
    /// Sequence start, or right after a globstar element.
    Fresh,
    /// Just consumed a `Sep`.
    Strict,
    /// Just consumed a `SepRun` (lenient `**` boundary).
    Lenient,
    /// A `GlobstarAny`/`SlashAnything` absorbs the rest of the path;
    /// any op after it makes the sequence non-expressible.
    Open,
}

fn segmentize(ops: &[Op], dot: bool, ci: bool) -> Option<ElemSeq> {
    let mut elems: Vec<Elem> = Vec::with_capacity(8);
    let mut buf: Vec<Op> = Vec::new();
    let mut state = Boundary::Fresh;

    for (i, op) in ops.iter().enumerate() {
        if state == Boundary::Open {
            return None;
        }
        match op {
            Op::Lit(_) | Op::AnyChar | Op::Star | Op::Class(_) | Op::Alternation(_) => {
                if lit_contains_sep(op) {
                    return None;
                }
                push_in_seg(&mut buf, op);
            }
            Op::Sep => {
                elems.push(close_segment(&mut buf, dot, ci)?);
                state = Boundary::Strict;
            }
            Op::SepRun => {
                elems.push(close_segment(&mut buf, dot, ci)?);
                state = Boundary::Lenient;
            }
            Op::LeadingSeps => {
                if i != 0 {
                    return None;
                }
            }
            Op::OptSegmentsSlash => {
                if !buf.is_empty() {
                    return None;
                }
                let strict_entry = match state {
                    Boundary::Fresh => !elems.is_empty(),
                    Boundary::Strict => true,
                    Boundary::Lenient => false,
                    Boundary::Open => unreachable!("handled at the loop top"),
                };
                elems.push(if strict_entry {
                    Elem::G0Strict
                } else {
                    Elem::G0
                });
                state = Boundary::Fresh;
            }
            Op::SlashAnything => {
                elems.push(close_segment(&mut buf, dot, ci)?);
                elems.push(Elem::G1);
                state = Boundary::Open;
            }
            Op::GlobstarAny => {
                if !buf.is_empty() {
                    return None;
                }
                let strict = state == Boundary::Strict;
                elems.push(if strict { Elem::G1 } else { Elem::G0 });
                state = Boundary::Open;
            }
            Op::Globstar => return None,
        }
    }

    if state != Boundary::Open {
        elems.push(close_segment(&mut buf, dot, ci)?);
    }
    finish(elems)
}

fn lit_contains_sep(op: &Op) -> bool {
    match op {
        Op::Lit(bytes) => bytes.iter().any(|&b| std::path::is_separator(b as char)),
        Op::Alternation(branches) => branches.iter().any(|b| b.iter().any(lit_contains_sep)),
        _ => false,
    }
}

fn push_in_seg(buf: &mut Vec<Op>, op: &Op) {
    if let (Op::Lit(bytes), Some(Op::Lit(prev))) = (op, buf.last_mut()) {
        prev.extend_from_slice(bytes);
        return;
    }
    buf.push(op.clone());
}

fn close_segment(buf: &mut Vec<Op>, dot: bool, ci: bool) -> Option<Elem> {
    if buf.is_empty() {
        return Some(Elem::Lit(Box::from(&b""[..])));
    }
    let mut ops = std::mem::take(buf);
    if ops.len() == 1 {
        if let Op::Lit(bytes) = &mut ops[0] {
            return Some(Elem::Lit(std::mem::take(bytes).into_boxed_slice()));
        }
    }
    Some(Elem::Wild(compile_wild(&ops, dot, ci)?))
}

fn compile_wild(ops: &[Op], dot: bool, ci: bool) -> Option<Wild> {
    let mut idx = 0;
    let prefix: &[u8] = match ops.first() {
        Some(Op::Lit(b)) => {
            idx = 1;
            b
        }
        _ => b"",
    };
    let mut anychars = 0u32;
    let mut has_star = false;
    while idx < ops.len() {
        match &ops[idx] {
            Op::Star => has_star = true,
            Op::AnyChar => anychars += 1,
            _ => break,
        }
        idx += 1;
    }
    let has_wilds = has_star || anychars > 0;
    let dot_protect = !dot && prefix.is_empty() && has_wilds;

    if idx == ops.len() {
        return Some(Wild {
            kind: WildKind::Affix {
                prefix: Box::from(prefix),
                suffix: Box::from(&b""[..]),
            },
            min_len: prefix.len() as u32 + anychars,
            variable: has_star,
            dot_protect,
        });
    }
    if let Some(suffixes) = suffix_product(&ops[idx..]) {
        if suffixes.len() == 1 {
            let suffix = suffixes.into_iter().next().unwrap();
            return Some(Wild {
                min_len: (prefix.len() + suffix.len()) as u32 + anychars,
                kind: WildKind::Affix {
                    prefix: Box::from(prefix),
                    suffix: suffix.into_boxed_slice(),
                },
                variable: has_star,
                dot_protect,
            });
        }
        return Some(Wild {
            kind: WildKind::AffixSet {
                prefix: Box::from(prefix),
                suffixes: suffixes
                    .into_iter()
                    .map(Vec::into_boxed_slice)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            },
            min_len: prefix.len() as u32 + anychars,
            variable: has_star,
            dot_protect,
        });
    }
    let nfa = SegNfa::compile(ops, dot, ci)?;
    let dot_protect = !dot && nfa.wild_led;
    Some(Wild {
        kind: WildKind::Generic(nfa),
        min_len: 0,
        variable: true,
        dot_protect,
    })
}

const MAX_SUFFIX_PRODUCT: usize = 16;

fn suffix_product(ops: &[Op]) -> Option<Vec<Vec<u8>>> {
    if let [Op::Lit(bytes)] = ops {
        return Some(vec![bytes.clone()]);
    }
    let mut parts: Vec<Vec<u8>> = vec![Vec::new()];
    for op in ops {
        match op {
            Op::Lit(bytes) => {
                for p in parts.iter_mut() {
                    p.extend_from_slice(bytes);
                }
            }
            Op::Alternation(branches) => {
                let mut lits = Vec::with_capacity(branches.len());
                for b in branches {
                    match b.as_slice() {
                        [] => lits.push(&b""[..]),
                        [Op::Lit(bytes)] => lits.push(bytes.as_slice()),
                        _ => return None,
                    }
                }
                if parts.len() * lits.len() > MAX_SUFFIX_PRODUCT {
                    return None;
                }
                let mut next = Vec::with_capacity(parts.len() * lits.len());
                for p in &parts {
                    for l in &lits {
                        let mut v = Vec::with_capacity(p.len() + l.len());
                        v.extend_from_slice(p);
                        v.extend_from_slice(l);
                        next.push(v);
                    }
                }
                parts = next;
            }
            _ => return None,
        }
    }
    Some(parts)
}

fn finish(elems: Vec<Elem>) -> Option<ElemSeq> {
    let m = elems.len();
    let mut state_of = Vec::with_capacity(m);
    let mut n: usize = 0;
    for e in &elems {
        state_of.push(n as u8);
        n += match e {
            Elem::G0Strict | Elem::G1 => 2,
            _ => 1,
        };
        if n >= MAX_SEQ_STATES {
            return None;
        }
    }
    let accept = n;
    n += 1;

    let mut elem_of = vec![0u8; n];
    for (i, &entry) in state_of.iter().enumerate() {
        let end = if i + 1 < m {
            state_of[i + 1] as usize
        } else {
            accept
        };
        elem_of[entry as usize..end].fill(i as u8);
    }

    let mut eps: Vec<u64> = (0..n).map(|s| 1u64 << s).collect();
    for i in (0..m).rev() {
        let s = state_of[i] as usize;
        let next_entry = if i + 1 < m {
            state_of[i + 1] as usize
        } else {
            accept
        };
        match elems[i] {
            Elem::G0 => eps[s] |= eps[next_entry],
            Elem::G0Strict => {
                eps[s] |= eps[next_entry];
                eps[s + 1] |= eps[next_entry];
            }
            Elem::G1 => {
                eps[s + 1] |= eps[next_entry];
            }
            _ => {}
        }
    }

    let mut reach1: u64 = 0;
    let mut sat_tail = true;
    for i in (0..m).rev() {
        let sat_i = match &elems[i] {
            Elem::Wild(w) => match &w.kind {
                WildKind::Generic(nfa) => nfa.satisfiable,
                _ => true,
            },
            _ => true,
        };
        let s = state_of[i] as usize;
        let can = match &elems[i] {
            Elem::G0 | Elem::G0Strict | Elem::G1 => sat_tail,
            Elem::Lit(_) | Elem::Wild(_) => sat_i && sat_tail,
        };
        if can {
            reach1 |= 1u64 << s;
            if matches!(elems[i], Elem::G0Strict | Elem::G1) {
                reach1 |= 1u64 << (s + 1);
            }
        }
        sat_tail = sat_i && sat_tail;
    }

    let g_count = elems.iter().filter(|e| e.is_globstar()).count() as u8;
    let single_g = if g_count == 1 {
        elems.iter().position(Elem::is_globstar).unwrap() as u8
    } else {
        u8::MAX
    };

    let mut joined_head = Vec::new();
    if g_count == 1 && single_g > 0 {
        let head = &elems[..single_g as usize];
        if head.iter().all(|e| matches!(e, Elem::Lit(_))) {
            for e in head {
                if let Elem::Lit(bytes) = e {
                    joined_head.extend_from_slice(bytes);
                    joined_head.push(b'/');
                }
            }
        }
    }

    let quick_suffix: Box<[u8]> = match elems.last() {
        Some(Elem::Lit(bytes)) => bytes.clone(),
        Some(Elem::Wild(w)) => match &w.kind {
            WildKind::Affix { suffix, .. } => suffix.clone(),
            _ => Box::from(&b""[..]),
        },
        _ => Box::from(&b""[..]),
    };

    Some(ElemSeq {
        elems: elems.into_boxed_slice(),
        single_g,
        g_count,
        joined_head: joined_head.into_boxed_slice(),
        state_of: state_of.into_boxed_slice(),
        elem_of: elem_of.into_boxed_slice(),
        num_states: n as u8,
        eps: eps.into_boxed_slice(),
        reach1,
        quick_suffix,
    })
}
