//! Ops → element sequences: fork expansion, the segmentizer, and
//! in-segment wildcard classification.

use crate::engine::ops::Op;

use super::seg_nfa::SegNfa;
use super::{Elem, ElemSeq, MAX_FORKS, MAX_SEQ_STATES, Wild, WildKind, is_sep};

/// Compile the lowered ops into fork sequences. `None` ⇒ not
/// segment-expressible (caller falls back to the Pike VM).
///
/// The dominant no-crossing-brace path segmentizes the ops in place —
/// only genuine forks pay for expansion copies.
pub(super) fn compile_seqs(ops: &[Op], dot: bool, ci: bool) -> Option<Vec<ElemSeq>> {
    if !ops.iter().any(op_is_crossing_alt) {
        return Some(vec![segmentize(ops, dot, ci)?]);
    }
    let op_seqs = expand_forks(ops)?;
    let mut seqs = Vec::with_capacity(op_seqs.len());
    for fork in &op_seqs {
        seqs.push(segmentize(fork, dot, ci)?);
    }
    Some(seqs)
}

/// Expand separator-crossing brace alternations into flat op
/// sequences (cartesian across multiple crossing braces, capped at
/// [`MAX_FORKS`]). In-segment alternations stay inline. `None` on cap
/// overflow.
fn expand_forks(ops: &[Op]) -> Option<Vec<Vec<Op>>> {
    if !ops.iter().any(op_is_crossing_alt) {
        // Recursion base: a branch without crossing braces is one
        // ready-made sequence.
        return Some(vec![ops.to_vec()]);
    }
    let mut seqs: Vec<Vec<Op>> = vec![Vec::with_capacity(ops.len())];
    for op in ops {
        if op_is_crossing_alt(op) {
            let Op::Alternation(branches) = op else {
                unreachable!()
            };
            // Each branch may itself need expansion (nested crossing
            // braces).
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

/// Does this op force a fork (an alternation containing separators /
/// globstar forms at any depth)?
fn op_is_crossing_alt(op: &Op) -> bool {
    match op {
        Op::Alternation(branches) => branches.iter().any(|b| b.iter().any(op_crosses_segment)),
        _ => false,
    }
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

/// Boundary state between elements while segmentizing. Invariant:
/// `state == InSegment` ⇔ the in-segment buffer is nonempty.
#[derive(PartialEq, Clone, Copy)]
enum Boundary {
    /// Sequence start, or right after a globstar boundary — at a
    /// segment start with no pending separator obligation.
    Fresh,
    /// A strict `Sep` was just consumed.
    Strict,
    /// A lenient `SepRun` was just consumed (native `**` boundary).
    Lenient,
    /// Accumulating in-segment ops.
    InSegment,
}

/// Lower one flat op sequence into an [`ElemSeq`]. `None` ⇒ not
/// segment-expressible (caller falls back to the Pike VM).
///
/// The tricky rows are splices — fork expansion creates adjacencies
/// the globstar fold never saw. Since GLOB_SPEC §7.0 (expansion
/// equation) the lowering distributes brace-flanking separators into
/// globstar-edged branches, so these shapes now only arise from the
/// shared-separator corner (`{a,**}/{**,b}`: the middle `/` is owned
/// by the left brace, leaving the right branch's absorber bare):
///
/// - `[…, Sep, GlobstarAny]` — the strict separator demands ≥ 1
///   absorbed segment: **G1**.
/// - `[GlobstarAny, Sep, …]` — the separator after `.*` likewise
///   forces ≥ 1 absorbed segment: upgrade to **G1**.
/// - `[…, Sep, OSS, …]` — OSS behind a strict `Sep` has no
///   separator-run leniency: **G0Strict** (no leading empty absorbed
///   segment).
/// - `GlobstarAny`/`SlashAnything` glued to in-segment ops — `.*`
///   ends mid-segment: fallback.
fn segmentize(ops: &[Op], dot: bool, ci: bool) -> Option<ElemSeq> {
    let mut elems: Vec<Elem> = Vec::with_capacity(8);
    let mut buf: Vec<Op> = Vec::new();
    let mut state = Boundary::Fresh;
    // Set right after emitting a globstar element whose op form does
    // not self-delimit (`GlobstarAny` / `SlashAnything`): the next op
    // decides how the absorber's right edge composes.
    let mut g_open = false;
    // The open globstar came from `GlobstarAny` at a lenient boundary
    // and upgrades G0 → G1 if a `Sep` follows.
    let mut g_upgradeable = false;
    let mut leading_seps = false;

    for (i, op) in ops.iter().enumerate() {
        debug_assert_eq!(buf.is_empty(), state != Boundary::InSegment);
        match op {
            Op::Lit(_) | Op::AnyChar | Op::Star | Op::Class(_) | Op::Alternation(_) => {
                debug_assert!(!op_is_crossing_alt(op), "forks expanded before segmentize");
                if g_open {
                    return None; // `.*` glued to segment content
                }
                // An escaped separator (`a\/b*`) embeds a real `Seps`
                // byte inside a Lit, where the byte engines match it
                // byte-exactly against a path separator. Segments are
                // sep-free by construction, so such literals are not
                // segment-expressible — fall back.
                if lit_contains_sep(op) {
                    return None;
                }
                push_in_seg(&mut buf, op);
                state = Boundary::InSegment;
            }
            Op::Sep => {
                if g_open {
                    // The separator is the open absorber's right
                    // boundary; a `.*` in front of a mandatory `/`
                    // must absorb at least one segment.
                    if g_upgradeable {
                        *elems.last_mut().unwrap() = Elem::G1;
                    }
                    g_open = false;
                    g_upgradeable = false;
                    state = Boundary::Fresh;
                } else {
                    elems.push(close_segment(&mut buf, dot, ci)?);
                    state = Boundary::Strict;
                }
            }
            Op::SepRun => {
                // Generated only immediately before an OSS.
                if g_open {
                    return None;
                }
                elems.push(close_segment(&mut buf, dot, ci)?);
                state = Boundary::Lenient;
            }
            Op::LeadingSeps => {
                if i != 0 {
                    return None;
                }
                leading_seps = true;
            }
            Op::OptSegmentsSlash => {
                if state == Boundary::InSegment || g_open {
                    return None; // glued (`x{**/a,b}`, `{**,x}{**/a,b}`)
                }
                let strict_entry = match state {
                    // Pattern-head OSS always carries LeadingSeps; a
                    // spliced head-of-branch OSS deeper in the
                    // sequence only ever follows a boundary op.
                    Boundary::Fresh => !leading_seps && !elems.is_empty(),
                    Boundary::Strict => true,
                    Boundary::Lenient => false,
                    Boundary::InSegment => unreachable!(),
                };
                elems.push(if strict_entry {
                    Elem::G0Strict
                } else {
                    Elem::G0
                });
                state = Boundary::Fresh;
                leading_seps = false;
            }
            Op::SlashAnything => {
                if g_open {
                    return None;
                }
                // Trailing `/**`: brings its own leading boundary.
                elems.push(close_segment(&mut buf, dot, ci)?);
                elems.push(Elem::G1);
                g_open = true;
                g_upgradeable = false;
                state = Boundary::Fresh;
            }
            Op::GlobstarAny => {
                if state == Boundary::InSegment || g_open {
                    return None; // glued (`a{**,x}b`)
                }
                // Behind a strict separator the absorber must consume
                // ≥ 1 segment (`a/{**,x}` rejects `a`).
                let strict = state == Boundary::Strict;
                elems.push(if strict { Elem::G1 } else { Elem::G0 });
                g_open = true;
                g_upgradeable = !strict;
                state = Boundary::Fresh;
            }
            Op::Globstar => return None, // never survives the fold
        }
    }

    if !g_open {
        // Close the final segment. A trailing boundary (`a/`,
        // `a/**/`) leaves an empty buffer and correctly emits
        // `Lit("")`.
        elems.push(close_segment(&mut buf, dot, ci)?);
    }
    finish(elems)
}

/// Does this in-segment op (or any nested alternation branch) hold a
/// literal byte from the `Seps` set? Only escapes can produce one
/// (`\/` always; `\\` on Windows).
fn lit_contains_sep(op: &Op) -> bool {
    match op {
        Op::Lit(bytes) => bytes.iter().any(|&b| is_sep(b)),
        Op::Alternation(branches) => branches.iter().any(|b| b.iter().any(lit_contains_sep)),
        _ => false,
    }
}

/// Append an in-segment op to the buffer, merging adjacent literals
/// (splices can produce `Lit,Lit` runs the lowering never sees).
fn push_in_seg(buf: &mut Vec<Op>, op: &Op) {
    if let (Op::Lit(bytes), Some(Op::Lit(prev))) = (op, buf.last_mut()) {
        prev.extend_from_slice(bytes);
        return;
    }
    buf.push(op.clone());
}

/// Convert the accumulated in-segment ops into an element.
fn close_segment(buf: &mut Vec<Op>, dot: bool, ci: bool) -> Option<Elem> {
    if buf.is_empty() {
        return Some(Elem::Lit(Box::from(&b""[..])));
    }
    let mut ops = std::mem::take(buf);
    if ops.len() == 1 {
        if let Op::Lit(bytes) = &mut ops[0] {
            // Move, don't clone — `ops` is owned and dropped here.
            return Some(Elem::Lit(std::mem::take(bytes).into_boxed_slice()));
        }
    }
    Some(Elem::Wild(compile_wild(&ops, dot, ci)?))
}

/// Classify in-segment ops into a [`Wild`].
fn compile_wild(ops: &[Op], dot: bool, ci: bool) -> Option<Wild> {
    // Shape scan: optional leading Lit, then a run of Star/AnyChar,
    // then either nothing, a trailing Lit, or a trailing all-literal
    // alternation. Anything else → Generic.
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
    // Wildcard-led matchers reject dot-led segments under dot=false.
    // A pure literal-set matcher (`{tob,crazy}`, no leading wilds) is
    // literal-led per branch and never protected.
    let dot_protect = !dot && prefix.is_empty() && has_wilds;

    if idx == ops.len() {
        // `lit`, `lit*??`, `*`, `???` — affix with empty suffix.
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
    // Trailing suffix product: everything after the wild run must be
    // Lit / all-literal Alternation ops. `*.{ts,tsx}` (Star, Lit ".",
    // Alt) glues to {".ts", ".tsx"}; `{tob,crazy}` (no wilds) becomes
    // an exact literal set via `variable=false`.
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

/// Cap on the suffix-product breadth (`*{a,b}{c,d}{e,f}{g,h}` = 16).
/// Wider products stay in the Generic NFA where cost is linear.
const MAX_SUFFIX_PRODUCT: usize = 16;

/// Cartesian product of trailing Lit / all-literal-Alternation ops.
/// `None` when any op is non-literal or the product exceeds the cap.
fn suffix_product(ops: &[Op]) -> Option<Vec<Vec<u8>>> {
    // Overwhelmingly common tail: one literal op (`*.ts`).
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

/// Build the element-NFA metadata and wrap up.
fn finish(elems: Vec<Elem>) -> Option<ElemSeq> {
    let m = elems.len();
    // Assign states: one entry state per element; G0Strict and G1 own
    // a second (body) state. Accept state last.
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

    // Inverse map: owning element per state (accept slot unused).
    let mut elem_of = vec![0u8; n];
    for (i, &entry) in state_of.iter().enumerate() {
        let end = if i + 1 < m {
            state_of[i + 1] as usize
        } else {
            accept
        };
        elem_of[entry as usize..end].fill(i as u8);
    }

    // ε-closures: globstar states that may stop absorbing skip to the
    // next element's entry. Right-to-left so skips chain.
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
                // Entry must absorb ≥ 1 — no skip; body may stop.
                eps[s + 1] |= eps[next_entry];
            }
            _ => {}
        }
    }

    // sat_from[i] — elements i.. can match SOME segment sequence.
    let mut sat_from = vec![true; m + 1];
    for i in (0..m).rev() {
        sat_from[i] = elem_satisfiable(&elems[i]) && sat_from[i + 1];
    }
    // Per-state "can consume ≥ 1 further segment on a path to
    // accept" (the `match_dir` prefix bit).
    let mut reach1: u64 = 0;
    for i in 0..m {
        let s = state_of[i] as usize;
        let can = match &elems[i] {
            Elem::G0 | Elem::G0Strict | Elem::G1 => sat_from[i + 1],
            Elem::Lit(_) | Elem::Wild(_) => sat_from[i],
        };
        if can {
            reach1 |= 1u64 << s;
            if matches!(elems[i], Elem::G0Strict | Elem::G1) {
                reach1 |= 1u64 << (s + 1);
            }
        }
    }

    let g_count = elems.iter().filter(|e| e.is_globstar()).count();
    let single_g = if g_count == 1 {
        elems.iter().position(Elem::is_globstar).unwrap()
    } else {
        usize::MAX
    };

    // Pre-join all-literal heads for the single-globstar fast path.
    let mut joined_head = Vec::new();
    if g_count == 1 && single_g > 0 {
        let all_lit = elems[..single_g].iter().all(|e| matches!(e, Elem::Lit(_)));
        if all_lit {
            for e in &elems[..single_g] {
                if let Elem::Lit(bytes) = e {
                    joined_head.extend_from_slice(bytes);
                    joined_head.push(b'/');
                }
            }
        }
    }

    // Per-fork quick-reject suffix from the final element.
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

/// Is there ANY segment this element consumes / absorbs?
fn elem_satisfiable(e: &Elem) -> bool {
    match e {
        Elem::Lit(_) | Elem::G0 | Elem::G0Strict | Elem::G1 => true,
        Elem::Wild(w) => match &w.kind {
            // Affix shapes are always satisfiable: wildcard-led ones
            // by a non-dot-led segment, literal-led ones by their own
            // literal (a leading literal `.` is never dot-protected).
            WildKind::Affix { .. } | WildKind::AffixSet { .. } => true,
            WildKind::Generic(nfa) => nfa.satisfiable,
        },
    }
}
