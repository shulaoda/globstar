//! SSM — segment-structured matcher. Primary engine for every pattern
//! whose ops decompose into a linear sequence of segment-shaped
//! elements (see `references/decisions/segment-engine-design.md`).
//!
//! A pattern compiles to one or more **element sequences** (more than
//! one only when a brace alternation crosses a `/`). Each element is
//! a literal segment, an in-segment wildcard matcher, or a globstar
//! absorber. Matching walks the path segment-at-a-time:
//!
//! - fixed-depth patterns: positional compare, one element per segment;
//! - single-globstar patterns (the dominant real-world shape): anchored
//!   head + anchored tail, the globstar absorbs the middle — no search;
//! - everything else: a tiny NFA over element positions, stepped once
//!   per *segment* (not per byte), active set in one `u64`.
//!
//! Compile is a linear scan over the lowered ops — no subset
//! construction, no hash maps, no ε-closure tables. Patterns the
//! segment model cannot express (globstars glued mid-segment via
//! braces, fork/element/state counts over the u64 budget) return the
//! program back so the caller can fall through to the automata tiers.

use globstar::ast::{CharClass, ClassItem};
use globstar::dir_match::DirMatch;
use globstar::engine::eq_byte;
use globstar::engine::facts::LiteralFacts;
use globstar::engine::ops::{Op, OpProgram, compute_static_prefixes};

/// Fork budget: a pattern whose separator-crossing brace expansion
/// yields more element sequences than this falls back to the automata
/// tiers. Real patterns hold one or two forks; the cap only guards
/// adversarial `{a/b,c/d}{e/f,g/h}...` products.
const MAX_FORKS: usize = 64;

/// Element-NFA state budget per sequence (active set is one `u64`).
const MAX_SEQ_STATES: usize = 64;

/// In-segment NFA state budget (active set is one `u64`).
const MAX_SEG_NFA_STATES: usize = 64;

#[inline(always)]
fn is_sep(b: u8) -> bool {
    std::path::is_separator(b as char)
}

// ---------------------------------------------------------------------------
// Compiled shape
// ---------------------------------------------------------------------------

/// One element of a sequence.
#[derive(Debug, Clone)]
enum Elem {
    /// Literal segment (possibly empty — `a//b` and trailing-`/`
    /// patterns produce empty literal segments). Never contains a
    /// separator byte.
    Lit(Box<[u8]>),
    /// In-segment wildcard matcher.
    Wild(Wild),
    /// Globstar absorbing ≥ 0 segments of any content (empty segments
    /// included). From `**/…`, mid `/**/`, and pattern-level `**`.
    G0,
    /// `G0` whose absorbed run may not *begin* with an empty segment.
    /// Arises when a brace fork splices a branch-internal `**/` behind
    /// a strict `Sep` (`a/{**/b,c}`), where the separator-run leniency
    /// of a native `/**/` boundary is absent.
    G0Strict,
    /// Globstar absorbing ≥ 1 segment. From trailing `/**`
    /// (`a/**` matches `a/` but not `a`) and from spliced bare `**`
    /// that sits behind or in front of a mandatory separator.
    G1,
}

impl Elem {
    #[inline]
    fn is_globstar(&self) -> bool {
        matches!(self, Elem::G0 | Elem::G0Strict | Elem::G1)
    }
}

/// In-segment matcher, classified at compile time.
#[derive(Debug, Clone)]
struct Wild {
    kind: WildKind,
    /// Minimum segment byte length (literal parts + `?` count). For
    /// [`WildKind::AffixSet`] this excludes the per-branch suffix.
    min_len: u32,
    /// `false` ⇒ the length must equal the minimum exactly (no `*`).
    variable: bool,
    /// Reject dot-led segments (only ever set on `dot=false`
    /// compiles, and only when the matcher is wildcard-led — its
    /// first byte cannot come from a literal or a positive class).
    dot_protect: bool,
}

#[derive(Debug, Clone)]
enum WildKind {
    /// `lit1 (*|?)+ lit2` and degenerate forms: pure `*`/`?` runs
    /// (both affixes empty), `*lit`, `lit*`, `a?b` (no star — exact
    /// length via `variable=false`), plain multi-op literals.
    Affix { prefix: Box<[u8]>, suffix: Box<[u8]> },
    /// `lit (*|?)+ {lit,…}` — segment must carry the prefix and end
    /// with one of the suffixes (`*.{ts,tsx}`).
    AffixSet {
        prefix: Box<[u8]>,
        suffixes: Box<[Box<[u8]>]>,
    },
    /// Everything else (classes, non-tail alternations, interior
    /// literal islands): a mini Thompson NFA over the in-segment ops,
    /// simulated with a `u64` active set.
    Generic(SegNfa),
}

/// One compiled fork sequence plus its precomputed NFA metadata.
#[derive(Debug, Clone)]
struct ElemSeq {
    elems: Box<[Elem]>,
    /// Index of the single globstar element when the sequence has
    /// exactly one (fast anchored path); `usize::MAX` otherwise.
    single_g: usize,
    /// Number of globstar elements.
    g_count: usize,
    /// Single-globstar fast path: when every head element is a `Lit`,
    /// the joined head bytes (`"src/"` — each segment plus one `/`),
    /// so the head check is one sep-aware compare instead of a
    /// segment iteration. Empty when `g == 0` or a head element is
    /// non-literal.
    joined_head: Box<[u8]>,
    /// `joined_head` is usable (all head elements literal).
    head_joined: bool,
    /// Element-NFA: per-element entry state id (ascending). Two-state
    /// elements (`G0Strict`, `G1`) own `entry` and `entry + 1`
    /// (body). The accept state is `num_states - 1`.
    state_of: Box<[u8]>,
    num_states: u8,
    /// `eps[s]` — states reachable from `s` via zero segment
    /// consumption (globstar "absorb nothing" skips), incl. `s`.
    eps: Box<[u64]>,
    /// States from which ≥ 1 further segment can be consumed on a
    /// path to accept. Drives `match_dir`'s prefix bit.
    reach1: u64,
    /// Per-fork quick reject: a byte suffix every match of THIS fork
    /// must end with (from a trailing `Lit` or `Affix` suffix; empty
    /// = no fact). Only consulted for multi-fork matchers, where the
    /// shared facts prefilter can't discriminate between forks.
    quick_suffix: Box<[u8]>,
}

/// Compiled segment-structured matcher.
#[derive(Debug, Clone)]
pub struct SegmentMatcher {
    seqs: Box<[ElemSeq]>,
    facts: LiteralFacts,
    prefixes: Box<[Box<[u8]>]>,
    case_insensitive: bool,
    dot: bool,
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

impl SegmentMatcher {
    /// Try to compile `program`. Returns the program back when the
    /// pattern is not segment-expressible so the caller can fall
    /// through to the automata tiers without re-lowering.
    pub fn build(program: OpProgram, dot: bool) -> Result<Box<Self>, OpProgram> {
        let ci = program.case_insensitive;
        let Some(op_seqs) = expand_forks(&program.ops) else {
            return Err(program);
        };
        let mut seqs = Vec::with_capacity(op_seqs.len());
        for ops in &op_seqs {
            match segmentize(ops, dot, ci) {
                Some(seq) => seqs.push(seq),
                None => return Err(program),
            }
        }
        let prefixes = compute_static_prefixes(&program.ops);
        let facts = program.facts;
        Ok(Box::new(Self {
            seqs: seqs.into_boxed_slice(),
            facts,
            prefixes,
            case_insensitive: ci,
            dot,
        }))
    }

    pub fn static_prefixes(&self) -> &[Box<[u8]>] {
        &self.prefixes
    }

    /// Diagnostic: `(fork count, element count of first fork)`.
    pub fn shape(&self) -> (usize, usize) {
        (
            self.seqs.len(),
            self.seqs.first().map_or(0, |s| s.elems.len()),
        )
    }

    #[inline(always)]
    pub fn is_match(&self, path: &[u8]) -> bool {
        if !self.facts.accept(path) {
            return false;
        }
        if self.case_insensitive {
            self.is_match_slow::<true>(path)
        } else {
            self.is_match_slow::<false>(path)
        }
    }

    /// Post-prefilter body. Kept out of line so the (dominant) facts
    /// rejection path stays tiny inside `Glob::is_match`.
    fn is_match_slow<const CI: bool>(&self, path: &[u8]) -> bool {
        if self.seqs.len() == 1 {
            return seq_matches::<CI>(&self.seqs[0], path, self.dot);
        }
        // Multi-fork: each fork's own suffix fact rejects without the
        // tail scan-back (the shared facts prefilter only knows the
        // union of suffixes).
        self.seqs.iter().any(|seq| {
            let qs = &seq.quick_suffix;
            if !qs.is_empty() {
                let n = path.len();
                if n < qs.len() || !affix_eq::<CI>(qs, &path[n - qs.len()..]) {
                    return false;
                }
            }
            seq_matches::<CI>(seq, path, self.dot)
        })
    }

    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        let (mut exact, mut prefix) = (false, false);
        for seq in self.seqs.iter() {
            let (e, p) = if self.case_insensitive {
                seq_match_dir::<true>(seq, dir_path, self.dot)
            } else {
                seq_match_dir::<false>(seq, dir_path, self.dot)
            };
            exact |= e;
            prefix |= p;
            if exact && prefix {
                break;
            }
        }
        DirMatch::from_exact_prefix(exact, prefix)
    }
}

/// Expand separator-crossing brace alternations into flat op
/// sequences (cartesian across multiple crossing braces, capped at
/// [`MAX_FORKS`]). In-segment alternations stay inline. `None` on cap
/// overflow.
fn expand_forks(ops: &[Op]) -> Option<Vec<Vec<Op>>> {
    if !ops.iter().any(op_is_crossing_alt) {
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

/// Boundary state between elements while segmentizing.
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
/// segment-expressible (caller falls back to the automata tiers).
///
/// The tricky rows are splices — fork expansion creates adjacencies
/// the globstar fold never saw, and their semantics follow the byte
/// engines' strict-`Sep` composition rather than the lenient native
/// boundaries:
///
/// - `[…, Sep, GlobstarAny]` (`a/{**,x}`) — the strict separator
///   demands ≥ 1 absorbed segment: **G1** (`a/` matches, `a` not).
/// - `[GlobstarAny, Sep, …]` (`{**,x}/b`) — the separator after `.*`
///   likewise forces ≥ 1 absorbed segment: upgrade to **G1**.
/// - `[…, Sep, OSS, …]` (`a/{**/b,c}`) — OSS behind a strict `Sep`
///   has no separator-run leniency: **G0Strict** (no leading empty
///   absorbed segment: `a//b` does not match).
/// - `GlobstarAny`/`SlashAnything` glued to in-segment ops
///   (`a{**,x}b`, `{a/**,x}c`) — `.*` ends mid-segment: fallback.
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
                debug_assert!(!g_open, "SepRun after an open globstar cannot be lowered");
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
                if !buf.is_empty() || state == Boundary::InSegment || g_open {
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
                elems.push(if strict_entry { Elem::G0Strict } else { Elem::G0 });
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
                if !buf.is_empty() || state == Boundary::InSegment || g_open {
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
        Op::Alternation(branches) => branches
            .iter()
            .any(|b| b.iter().any(lit_contains_sep)),
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
    let ops = std::mem::take(buf);
    if let [Op::Lit(bytes)] = ops.as_slice() {
        return Some(Elem::Lit(bytes.clone().into_boxed_slice()));
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
                kind: WildKind::Affix {
                    prefix: Box::from(prefix),
                    suffix: Box::from(suffix.as_slice()),
                },
                min_len: (prefix.len() + suffix.len()) as u32 + anychars,
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
    let mut head_joined = false;
    if g_count == 1 && single_g > 0 {
        head_joined = elems[..single_g].iter().all(|e| matches!(e, Elem::Lit(_)));
        if head_joined {
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
        head_joined,
        state_of: state_of.into_boxed_slice(),
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

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Iterator over `(start, end)` byte ranges of a path's segments.
/// Splits on every `Seps` byte; the empty path yields one empty
/// segment.
#[derive(Clone, Copy)]
struct SegIter<'a> {
    path: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> SegIter<'a> {
    fn new(path: &'a [u8]) -> Self {
        Self {
            path,
            pos: 0,
            done: false,
        }
    }
}

impl<'a> Iterator for SegIter<'a> {
    type Item = (usize, usize);

    #[inline]
    fn next(&mut self) -> Option<(usize, usize)> {
        if self.done {
            return None;
        }
        let start = self.pos;
        let mut i = start;
        while i < self.path.len() && !is_sep(self.path[i]) {
            i += 1;
        }
        if i == self.path.len() {
            self.done = true;
        }
        // Advance unconditionally: after the final segment `pos` is
        // `len + 1`, a sentinel no real segment start can equal —
        // the single-globstar overlap check relies on it.
        self.pos = i + 1;
        Some((start, i))
    }
}

#[inline(always)]
fn seq_matches<const CI: bool>(seq: &ElemSeq, path: &[u8], dot: bool) -> bool {
    match seq.g_count {
        0 => match_fixed::<CI>(seq, path, dot),
        1 => match_single_g::<CI>(seq, path, dot),
        _ => nfa_run::<CI>(seq, path, dot) & accept_bit(seq) != 0,
    }
}

#[inline]
fn accept_bit(seq: &ElemSeq) -> u64 {
    1u64 << (seq.num_states - 1)
}

/// Fixed-depth: element count must equal segment count; positional
/// compare.
fn match_fixed<const CI: bool>(seq: &ElemSeq, path: &[u8], dot: bool) -> bool {
    let mut segs = SegIter::new(path);
    for e in seq.elems.iter() {
        let Some((s, t)) = segs.next() else {
            return false;
        };
        if !elem_consumes::<CI>(e, &path[s..t], dot) {
            return false;
        }
    }
    segs.next().is_none()
}

/// Single-globstar: anchored tail (checked first — it rejects
/// fastest), anchored head, globstar absorbs the middle. No
/// searching, and at most one pass over the head + tail byte ranges.
#[inline(always)]
fn match_single_g<const CI: bool>(seq: &ElemSeq, path: &[u8], dot: bool) -> bool {
    let g = seq.single_g;
    let m = seq.elems.len();
    let tail_len = m - g - 1;

    // Tail: the last `tail_len` elements against the last `tail_len`
    // segments, right-to-left. `ts` ends up at the byte start of the
    // FIRST tail segment.
    let mut tail_end = path.len();
    let mut ts = 0usize;
    for j in (0..tail_len).rev() {
        let mut s = tail_end;
        while s > 0 && !is_sep(path[s - 1]) {
            s -= 1;
        }
        if !elem_consumes::<CI>(&seq.elems[g + 1 + j], &path[s..tail_end], dot) {
            return false;
        }
        if j > 0 {
            if s == 0 {
                return false; // fewer segments than tail elements
            }
            tail_end = s - 1;
        }
        ts = s;
    }

    // Head: elements 0..g against the first g segments. All-literal
    // heads (`src/**/…`) compare as one pre-joined sep-aware prefix.
    let mid_start;
    let head_exhausted;
    if seq.head_joined {
        let head = &seq.joined_head;
        // The joined head includes the separator after each head
        // segment; when the path lacks that final separator the
        // sequence can never match (the globstar and any tail all
        // sit beyond it) — same verdict the arity checks below give
        // on the iterator path.
        if path.len() < head.len() {
            return false;
        }
        for (i, &hb) in head.iter().enumerate() {
            let pb = path[i];
            let ok = if hb == b'/' {
                is_sep(pb)
            } else {
                eq_byte::<CI>(hb, pb)
            };
            if !ok {
                return false;
            }
        }
        mid_start = head.len();
        head_exhausted = false;
    } else {
        let mut iter = SegIter::new(path);
        for e in seq.elems[..g].iter() {
            let Some((s, t)) = iter.next() else {
                return false;
            };
            if !elem_consumes::<CI>(e, &path[s..t], dot) {
                return false;
            }
        }
        // Start of segment g — `len + 1` sentinel when the path ran
        // out of segments (see `SegIter::next`), which fails the
        // overlap check below whenever the tail (or G1) still needs
        // one.
        mid_start = iter.pos;
        head_exhausted = iter.done;
    }

    // Overlap / arity check and the absorbed middle's byte range.
    let (mid_exists, mid_end) = if tail_len > 0 {
        if ts < mid_start {
            return false; // head and tail would share segments
        }
        (ts > mid_start, ts.saturating_sub(1))
    } else {
        (!head_exhausted, path.len())
    };

    match seq.elems[g] {
        Elem::G0 => {}
        Elem::G0Strict => {
            // First absorbed segment must be nonempty: empty ⇔ the
            // range starts at path end or on a separator.
            if mid_exists && (mid_start >= path.len() || is_sep(path[mid_start])) {
                return false;
            }
        }
        Elem::G1 => {
            if !mid_exists {
                return false;
            }
        }
        _ => unreachable!(),
    }

    // Dot rule over the absorbed middle (dot=false compiles only).
    if dot || !mid_exists {
        return true;
    }
    !(mid_start <= mid_end && has_dot_led_segment(path, mid_start, mid_end))
}

/// Any nonempty segment beginning in `[start, end)` that starts with
/// `.`? Segments begin at `start` and after every separator.
#[inline]
fn has_dot_led_segment(path: &[u8], start: usize, end: usize) -> bool {
    if start < end && path[start] == b'.' {
        return true;
    }
    let mut i = start;
    while i < end {
        if is_sep(path[i]) && i + 1 < end && path[i + 1] == b'.' {
            return true;
        }
        i += 1;
    }
    false
}

/// General element-NFA run (multi-globstar `is_match` and every
/// `match_dir`). Returns the active state mask after consuming all
/// segments.
fn nfa_run<const CI: bool>(seq: &ElemSeq, path: &[u8], dot: bool) -> u64 {
    let mut active = seq.eps[seq.state_of[0] as usize];
    for (s, t) in SegIter::new(path) {
        if active == 0 {
            return 0;
        }
        active = nfa_step::<CI>(seq, active, &path[s..t], dot);
    }
    active
}

/// One segment step of the element NFA.
fn nfa_step<const CI: bool>(seq: &ElemSeq, active: u64, seg: &[u8], dot: bool) -> u64 {
    let mut next: u64 = 0;
    let m = seq.elems.len();
    let seg_dot_led = !seg.is_empty() && seg[0] == b'.';
    let absorb_ok = dot || !seg_dot_led;
    let mut bits = active;
    while bits != 0 {
        let s = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        if s as u8 == seq.num_states - 1 {
            continue; // accept has no outgoing transitions
        }
        let i = match seq.state_of.binary_search(&(s as u8)) {
            Ok(i) => i,
            Err(i) => i - 1, // body state of a two-state globstar
        };
        let entry = seq.state_of[i] as usize;
        let next_entry = if i + 1 < m {
            seq.state_of[i + 1] as usize
        } else {
            (seq.num_states - 1) as usize
        };
        match &seq.elems[i] {
            Elem::Lit(lit) => {
                if lit_eq::<CI>(lit, seg) {
                    next |= seq.eps[next_entry];
                }
            }
            Elem::Wild(w) => {
                if wild_consumes::<CI>(w, seg, dot) {
                    next |= seq.eps[next_entry];
                }
            }
            Elem::G0 => {
                if absorb_ok {
                    next |= seq.eps[entry];
                }
            }
            Elem::G0Strict => {
                let at_entry = s == entry;
                // Entry additionally demands a nonempty first
                // absorbed segment.
                if absorb_ok && !(at_entry && seg.is_empty()) {
                    next |= seq.eps[entry + 1];
                }
            }
            Elem::G1 => {
                if absorb_ok {
                    next |= seq.eps[entry + 1];
                }
            }
        }
    }
    next
}

/// `match_dir` for one sequence: `(exact, prefix)`.
fn seq_match_dir<const CI: bool>(seq: &ElemSeq, dir: &[u8], dot: bool) -> (bool, bool) {
    let active = nfa_run::<CI>(seq, dir, dot);
    let exact = active & accept_bit(seq) != 0;
    let prefix = active & seq.reach1 != 0;
    (exact, prefix)
}

// ---------------------------------------------------------------------------
// Element / wild consumption
// ---------------------------------------------------------------------------

#[inline(always)]
fn elem_consumes<const CI: bool>(e: &Elem, seg: &[u8], dot: bool) -> bool {
    match e {
        Elem::Lit(lit) => lit_eq::<CI>(lit, seg),
        Elem::Wild(w) => wild_consumes::<CI>(w, seg, dot),
        _ => false, // globstars are never positionally consumed
    }
}

#[inline]
fn lit_eq<const CI: bool>(lit: &[u8], seg: &[u8]) -> bool {
    if !CI {
        return lit == seg;
    }
    lit.len() == seg.len() && lit.iter().zip(seg).all(|(&a, &b)| eq_byte::<true>(a, b))
}

#[inline]
fn affix_eq<const CI: bool>(part: &[u8], seg_part: &[u8]) -> bool {
    debug_assert_eq!(part.len(), seg_part.len());
    if !CI {
        return part == seg_part;
    }
    part.iter()
        .zip(seg_part)
        .all(|(&a, &b)| eq_byte::<true>(a, b))
}

#[inline(always)]
fn wild_consumes<const CI: bool>(w: &Wild, seg: &[u8], _dot: bool) -> bool {
    if w.dot_protect && !seg.is_empty() && seg[0] == b'.' {
        return false;
    }
    let len = seg.len();
    match &w.kind {
        WildKind::Affix { prefix, suffix } => {
            let need = w.min_len as usize;
            if len < need || (!w.variable && len != need) {
                return false;
            }
            affix_eq::<CI>(prefix, &seg[..prefix.len()])
                && affix_eq::<CI>(suffix, &seg[len - suffix.len()..])
        }
        WildKind::AffixSet { prefix, suffixes } => {
            if len < prefix.len() || !affix_eq::<CI>(prefix, &seg[..prefix.len()]) {
                return false;
            }
            suffixes.iter().any(|suf| {
                let need = w.min_len as usize + suf.len();
                len >= need
                    && (w.variable || len == need)
                    && affix_eq::<CI>(suf, &seg[len - suf.len()..])
            })
        }
        WildKind::Generic(nfa) => nfa.matches(seg),
    }
}

// ---------------------------------------------------------------------------
// In-segment mini NFA (Generic wilds)
// ---------------------------------------------------------------------------

/// Thompson-lite over in-segment ops. ≤ 64 states, `u64` active set,
/// per-state successor ε-closures precomputed. Dot protection is
/// realized exactly as in the byte engines: offset 0 is the only
/// segment start inside a segment, so DotGuards block there when the
/// first byte is `.`, and dot-protected consumers refuse that `.`.
#[derive(Debug, Clone)]
struct SegNfa {
    states: Box<[SegState]>,
    /// ε-closure of the entry with DotGuards passable (used for all
    /// non-dot-led segments, and for the EOF/empty-segment accept).
    init: u64,
    /// ε-closure of the entry with DotGuards blocked (dot-led segment
    /// under a dot=false compile).
    init_dot_blocked: u64,
    /// Per-state successor ε-closure (guards pass — positions ≥ 1 are
    /// never segment starts).
    closures: Box<[u64]>,
    accept_mask: u64,
    /// Does the NFA accept any segment at all? (`match_dir`
    /// satisfiability.)
    satisfiable: bool,
    /// No entry-closure state can consume a leading `.` as a literal
    /// or positive class ⇒ the matcher is fully dot-protected.
    wild_led: bool,
    /// Compile-time dot option (drives the offset-0 gates).
    dot: bool,
}

#[derive(Debug, Clone)]
enum SegState {
    /// byte, next
    Byte(u8, u8),
    /// class, next, dot_protected (negated class under dot=false)
    Class(Box<CharClass>, u8, bool),
    /// next, dot_protected — `?` and star bodies (segments contain no
    /// separators, so "any byte" ≡ "any non-separator byte" here).
    Any(u8, bool),
    Split(u8, u8),
    Jump(u8),
    DotGuard(u8),
    Match,
}

const UNSET: u8 = u8::MAX;

impl SegNfa {
    fn compile(ops: &[Op], dot: bool, ci: bool) -> Option<Self> {
        let mut b = SegBuilder {
            states: Vec::with_capacity(16),
            tails: Vec::new(),
            ci,
        };
        let entry = b.compile_ops(ops, dot)?;
        let accept = b.alloc(SegState::Match)?;
        for t in std::mem::take(&mut b.tails) {
            b.patch(t, accept);
        }
        let states = b.states.into_boxed_slice();
        let n = states.len();

        // Memoized guard-passing closures — the ε-graph (Split/Jump/
        // Guard edges) is acyclic (every pattern loop passes through
        // a consumer), so each state folds from its children once.
        let mut closures = vec![u64::MAX; n];
        for s in 0..n {
            memo_closure(&states, &mut closures, s);
        }
        let init = closures[entry as usize];
        let init_dot_blocked = closure_of(&states, entry as usize, false);
        let accept_mask = 1u64 << (n - 1);

        // wild_led: can any entry state consume `.` as a literal or
        // positive class? If not, a dot-led segment can never match
        // under dot=false and the whole matcher is protected.
        let mut can_lit_dot = false;
        let mut bits = init;
        while bits != 0 {
            let s = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            match &states[s] {
                SegState::Byte(x, _) => can_lit_dot |= *x == b'.',
                SegState::Class(cls, _, dp) => can_lit_dot |= !*dp && cls.matches(b'.'),
                _ => {}
            }
        }
        let wild_led = !can_lit_dot;

        let satisfiable = compute_satisfiable(&states, &closures, init, accept_mask);

        Some(Self {
            states,
            init,
            init_dot_blocked,
            closures: closures.into_boxed_slice(),
            accept_mask,
            satisfiable,
            wild_led,
            dot,
        })
    }

    /// Match a whole segment.
    fn matches(&self, seg: &[u8]) -> bool {
        let protected_start = !self.dot && !seg.is_empty() && seg[0] == b'.';
        let mut active = if protected_start {
            self.init_dot_blocked
        } else {
            self.init
        };
        for (idx, &c) in seg.iter().enumerate() {
            if active == 0 {
                return false;
            }
            let guard_dot = idx == 0 && !self.dot && c == b'.';
            let mut next: u64 = 0;
            let mut bits = active;
            while bits != 0 {
                let s = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                match &self.states[s] {
                    SegState::Byte(b, nx) => {
                        if eq_byte::<false>(*b, c) {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    SegState::Class(cls, nx, dp) => {
                        if cls.matches(c) && !(*dp && guard_dot) {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    SegState::Any(nx, dp) => {
                        if !(*dp && guard_dot) {
                            next |= self.closures[*nx as usize];
                        }
                    }
                    _ => {}
                }
            }
            active = next;
        }
        active & self.accept_mask != 0
    }
}

/// Memoized guard-passing closure. `u64::MAX` marks "uncomputed" — a
/// real closure can never be all-ones (Split states are never closure
/// members, and a splitless NFA has single-bit closures).
fn memo_closure(states: &[SegState], memo: &mut [u64], s: usize) -> u64 {
    if memo[s] != u64::MAX {
        return memo[s];
    }
    let out = match &states[s] {
        SegState::Split(a, b) => {
            memo_closure(states, memo, *a as usize) | memo_closure(states, memo, *b as usize)
        }
        SegState::Jump(n) | SegState::DotGuard(n) => memo_closure(states, memo, *n as usize),
        _ => 1u64 << s,
    };
    memo[s] = out;
    out
}

/// ε-closure of `s` (consuming states + Match reachable through
/// Split/Jump, and DotGuard when `guards_pass`).
fn closure_of(states: &[SegState], s: usize, guards_pass: bool) -> u64 {
    let mut seen: u64 = 0;
    let mut out: u64 = 0;
    let mut stack = [0u8; 2 * MAX_SEG_NFA_STATES];
    let mut sp = 0usize;
    stack[sp] = s as u8;
    sp += 1;
    while sp > 0 {
        sp -= 1;
        let cur = stack[sp] as usize;
        if seen & (1u64 << cur) != 0 {
            continue;
        }
        seen |= 1u64 << cur;
        match &states[cur] {
            SegState::Split(a, b) => {
                stack[sp] = *a;
                sp += 1;
                stack[sp] = *b;
                sp += 1;
            }
            SegState::Jump(n) => {
                stack[sp] = *n;
                sp += 1;
            }
            SegState::DotGuard(n) => {
                if guards_pass {
                    stack[sp] = *n;
                    sp += 1;
                }
            }
            _ => out |= 1u64 << cur,
        }
    }
    out
}

/// Does the NFA accept any string at all? Fixpoint over consuming
/// states whose byte test is satisfiable by some byte. Per-state
/// satisfiability (the 256-scan for classes) and successor closures
/// are computed once, outside the fixpoint.
fn compute_satisfiable(states: &[SegState], closures: &[u64], init: u64, accept_mask: u64) -> bool {
    let n = states.len();
    let mut fire_next = [0u8; MAX_SEG_NFA_STATES];
    let mut fires: u64 = 0;
    for (s, st) in states.iter().enumerate() {
        let next = match st {
            SegState::Byte(_, n) | SegState::Any(n, _) => Some(*n),
            SegState::Class(cls, n, _) => {
                if (0u16..=255).any(|b| cls.matches(b as u8)) {
                    Some(*n)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(nx) = next {
            fires |= 1u64 << s;
            fire_next[s] = nx;
        }
    }
    let _ = n;
    let mut reach = init;
    loop {
        let mut grew = false;
        let mut bits = reach & fires;
        while bits != 0 {
            let s = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let clo = closures[fire_next[s] as usize];
            if reach | clo != reach {
                reach |= clo;
                grew = true;
            }
        }
        if !grew {
            return reach & accept_mask != 0;
        }
    }
}

struct SegBuilder {
    states: Vec<SegState>,
    tails: Vec<u8>,
    ci: bool,
}

impl SegBuilder {
    fn alloc(&mut self, s: SegState) -> Option<u8> {
        if self.states.len() >= MAX_SEG_NFA_STATES {
            return None;
        }
        self.states.push(s);
        Some((self.states.len() - 1) as u8)
    }

    fn patch(&mut self, state: u8, target: u8) {
        match &mut self.states[state as usize] {
            SegState::Byte(_, n)
            | SegState::Class(_, n, _)
            | SegState::Any(n, _)
            | SegState::Jump(n)
            | SegState::DotGuard(n) => {
                if *n == UNSET {
                    *n = target;
                }
            }
            SegState::Split(a, b) => {
                if *a == UNSET {
                    *a = target;
                }
                if *b == UNSET {
                    *b = target;
                }
            }
            SegState::Match => unreachable!(),
        }
    }

    fn compile_ops(&mut self, ops: &[Op], dot: bool) -> Option<u8> {
        if ops.is_empty() {
            let s = self.alloc(SegState::Jump(UNSET))?;
            self.tails.push(s);
            return Some(s);
        }
        let mut entry: Option<u8> = None;
        let mut pending: Vec<u8> = Vec::new();
        for op in ops {
            let (op_entry, mut op_tails) = self.compile_op(op, dot)?;
            for t in pending.drain(..) {
                self.patch(t, op_entry);
            }
            pending.append(&mut op_tails);
            if entry.is_none() {
                entry = Some(op_entry);
            }
        }
        self.tails.append(&mut pending);
        entry
    }

    fn compile_op(&mut self, op: &Op, dot: bool) -> Option<(u8, Vec<u8>)> {
        match op {
            Op::Lit(bytes) => {
                debug_assert!(!bytes.is_empty());
                let entry = self.lit_state(bytes[0])?;
                let mut prev = entry;
                for &b in &bytes[1..] {
                    let s = self.lit_state(b)?;
                    self.patch(prev, s);
                    prev = s;
                }
                Some((entry, vec![prev]))
            }
            Op::AnyChar => {
                let s = self.alloc(SegState::Any(UNSET, !dot))?;
                Some((s, vec![s]))
            }
            Op::Class(cls) => {
                let dp = !dot && cls.negated;
                let s = self.alloc(SegState::Class(Box::new(cls.clone()), UNSET, dp))?;
                Some((s, vec![s]))
            }
            Op::Star => {
                let entry = self.alloc(SegState::Split(UNSET, UNSET))?;
                let body = self.alloc(SegState::Any(entry, !dot))?;
                let exit = if !dot {
                    self.alloc(SegState::DotGuard(UNSET))?
                } else {
                    self.alloc(SegState::Jump(UNSET))?
                };
                if let SegState::Split(a, b) = &mut self.states[entry as usize] {
                    *a = body;
                    *b = exit;
                }
                Some((entry, vec![exit]))
            }
            Op::Alternation(branches) => {
                debug_assert!(!branches.is_empty());
                let mut entries = Vec::with_capacity(branches.len());
                let mut tails: Vec<u8> = Vec::new();
                for branch in branches {
                    let saved = std::mem::take(&mut self.tails);
                    let e = self.compile_ops(branch, dot)?;
                    let branch_tails = std::mem::replace(&mut self.tails, saved);
                    entries.push(e);
                    tails.extend(branch_tails);
                }
                let mut next_state: Option<u8> = None;
                for i in (0..branches.len().saturating_sub(1)).rev() {
                    let a = entries[i];
                    let b = next_state.unwrap_or(entries[i + 1]);
                    let s = self.alloc(SegState::Split(a, b))?;
                    next_state = Some(s);
                }
                Some((next_state.unwrap_or(entries[0]), tails))
            }
            // Separator-crossing ops never appear inside a segment.
            _ => None,
        }
    }

    fn lit_state(&mut self, b: u8) -> Option<u8> {
        let alt = globstar::options::ascii_case_alt(b);
        if self.ci && alt != b {
            let cls = CharClass {
                negated: false,
                items: vec![ClassItem::Byte(b), ClassItem::Byte(alt)],
            };
            self.alloc(SegState::Class(Box::new(cls), UNSET, false))
        } else {
            self.alloc(SegState::Byte(b, UNSET))
        }
    }
}
