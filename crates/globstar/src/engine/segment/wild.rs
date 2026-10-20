//! In-segment wildcard matchers: what a segment with a wildcard compiles
//! to (`compile_wild`) and how it takes a path segment
//! (`wild_consumes`). `wild_consumes` and `set_head` are forced inline
//! (see `chain.rs`).

use std::cmp::Ordering;

use crate::parse::ast::Node;

use super::seg_nfa::SegNfa;
use super::{Bytes, SegmentMatcher};

/// A 64-slot sketch of a set of bytes (`b % 64`): a byte not in the
/// sketch is not in the set, so a set scan can be skipped.
#[derive(Debug, Clone, Copy)]
pub(super) struct ByteSketch(u64);

impl ByteSketch {
    pub(super) fn of_last_bytes<'a>(strings: impl Iterator<Item = &'a [u8]>, ci: bool) -> Self {
        let mut bits = 0u64;
        for s in strings {
            match s.last() {
                // The empty string ends with anything.
                None => return ByteSketch(u64::MAX),
                Some(&b) => {
                    bits |= 1 << (b & 63);
                    if ci {
                        bits |= 1 << (b.to_ascii_lowercase() & 63);
                        bits |= 1 << (b.to_ascii_uppercase() & 63);
                    }
                    // A trailing `/` stands for any separator.
                    if b == b'/' {
                        bits |= 1 << (b'\\' & 63);
                    }
                }
            }
        }
        ByteSketch(bits)
    }

    #[inline]
    pub(super) fn may_end(self, s: &[u8]) -> bool {
        match s.last() {
            None => self.0 == u64::MAX,
            Some(&b) => self.0 & (1 << (b & 63)) != 0,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Wild {
    pub(super) kind: WildKind,
    /// Minimum segment byte length (literal parts + `?` count; for
    /// suffix sets excludes the per-branch suffix; `Generic` keeps 0, or
    /// `NEVER` when it accepts nothing).
    pub(super) min_len: u32,
    /// `false` ⇒ length must equal `min_len` exactly (no `*`).
    pub(super) variable: bool,
    /// Reject dot-led segments (wildcard-led matcher on a
    /// `dot=false` compile).
    pub(super) dot_protect: bool,
    /// Its wildcards alone match every non-empty segment (`*`, `?*`,
    /// `{a,*}`); drives `match_dir`'s all-below bit.
    pub(super) any_seg: bool,
}

impl Wild {
    /// What the kind's tables take: a set's suffixes (one byte more
    /// each), an NFA's states and kept closures. The compile walk
    /// charges it (`GlobError::TooComplex`), so a brace pattern whose
    /// segments multiply cannot keep big tables in each of them.
    pub(super) fn table_bytes(&self) -> usize {
        match &self.kind {
            WildKind::Affix(_) => 0,
            WildKind::AffixSet(set) => set.parts[1..].iter().map(|s| s.len() + 1).sum(),
            WildKind::AffixIndex(index) => {
                index.suffixes.len() + index.runs.iter().map(|r| r.count as usize).sum::<usize>()
            }
            WildKind::Generic(nfa) => nfa.table_bytes(),
        }
    }
}

/// The rarer kinds are boxed so an `Elem` stays 40 bytes.
#[derive(Debug, Clone)]
pub(super) enum WildKind {
    /// `lit (*|?)+ lit` and degenerate forms (`*`, `*lit`, `lit*`,
    /// `a?b`): compare the affixes, the length rules do the rest.
    Affix(Affix),
    /// `lit (*|?)+ {lit,…}` (`*.{ts,tsx}`): the prefix plus one
    /// suffix out of a set of at most `LINEAR_SUFFIXES`, scanned in
    /// pattern order.
    AffixSet(AffixSet),
    /// `AffixSet` past `LINEAR_SUFFIXES` (a factored union of names).
    AffixIndex(Box<AffixIndex>),
    /// Everything else: a mini Thompson NFA over the in-segment nodes.
    Generic(Box<SegNfa>),
}

/// A prefix and a suffix in one string, `split` bytes of prefix.
#[derive(Debug, Clone)]
pub(super) struct Affix {
    bytes: Bytes,
    split: u32,
}

impl Affix {
    pub(super) fn new(prefix: Bytes, suffix: Bytes) -> Self {
        let split = prefix.len() as u32;
        let bytes = match (prefix.is_empty(), suffix.is_empty()) {
            (true, _) => suffix,
            (_, true) => prefix,
            _ => [&prefix[..], &suffix[..]].concat().into_boxed_slice(),
        };
        Affix { bytes, split }
    }

    #[inline]
    pub(super) fn parts(&self) -> (&[u8], &[u8]) {
        self.bytes.split_at(self.split as usize)
    }
}

#[derive(Debug, Clone)]
pub(super) struct AffixSet {
    /// The prefix, then the suffixes.
    pub(super) parts: Box<[Bytes]>,
    pub(super) last: ByteSketch,
}

/// A large suffix set: sorted by (length, bytes), ASCII-lowercased
/// when case-insensitive, laid end to end in one allocation, with one
/// `SuffixRun` per length for a binary search by index arithmetic.
#[derive(Debug, Clone)]
pub(super) struct AffixIndex {
    pub(super) prefix: Bytes,
    pub(super) suffixes: Bytes,
    pub(super) last: ByteSketch,
    /// One run per distinct suffix length, ascending.
    pub(super) runs: Box<[SuffixRun]>,
}

/// `count` suffixes of `len` bytes each, from byte `start` on.
#[derive(Debug, Clone, Copy)]
pub(super) struct SuffixRun {
    pub(super) len: u32,
    pub(super) start: u32,
    pub(super) count: u32,
}

/// The bytes of a run of literal nodes, allocated once.
pub(super) fn concat_lits(nodes: &[&Node]) -> Bytes {
    fn lit(node: &Node) -> &[u8] {
        match node {
            Node::Literal(bytes) => bytes,
            _ => unreachable!("not a literal"),
        }
    }
    match nodes {
        [one] => Bytes::from(lit(one)),
        _ => {
            let mut acc = Vec::with_capacity(nodes.iter().map(|n| lit(n).len()).sum());
            for node in nodes {
                acc.extend_from_slice(lit(node));
            }
            acc.into_boxed_slice()
        }
    }
}

pub(super) fn compile_wild(nodes: &[&Node], dot: bool, ci: bool) -> Wild {
    let mut idx = nodes
        .iter()
        .take_while(|n| matches!(n, Node::Literal(_)))
        .count();
    let prefix = concat_lits(&nodes[..idx]);
    let mut anychars = 0u32;
    let mut has_star = false;
    while idx < nodes.len() {
        match nodes[idx] {
            Node::Star => has_star = true,
            Node::AnyChar => anychars += 1,
            _ => break,
        }
        idx += 1;
    }
    // A literal product after the wildcards (one empty suffix when
    // nothing follows them) keeps the segment anchored at both ends;
    // anything else runs the NFA.
    let Some(mut suffixes) = suffix_product(&nodes[idx..]) else {
        return SegNfa::wild(nodes, dot, ci);
    };
    let dot_protect = !dot && prefix.is_empty() && (has_star || anychars > 0);
    // `*` or `?*` with nothing literal around it, up to a suffix that
    // may be empty.
    let any_seg =
        prefix.is_empty() && has_star && anychars <= 1 && suffixes.iter().any(|s| s.is_empty());
    let mut min_len = prefix.len() as u32 + anychars;
    let kind = if suffixes.len() == 1 {
        let suffix = suffixes.pop().unwrap();
        min_len += suffix.len() as u32;
        WildKind::Affix(Affix::new(prefix, suffix.into_boxed_slice()))
    } else {
        affix_set(prefix, suffixes, ci)
    };
    Wild {
        min_len,
        kind,
        variable: has_star,
        dot_protect,
        any_seg,
    }
}

/// Literal suffix alternatives one segment enumerates (`*.{a,b,…}`, a
/// factored union of names). A product of alternatives (`{a,b}{c,d}…`)
/// past this many suffixes stays an NFA, which holds it without
/// multiplying it out; a single list is no product and is enumerated
/// whatever its length.
const MAX_SUFFIX_PRODUCT: usize = 1024;

/// An `AffixSet` up to this size is scanned in pattern order; a
/// larger one is sorted and binary-searched per suffix length.
const LINEAR_SUFFIXES: usize = 8;

/// The enumerated suffixes, one byte more each, may not take more than
/// this or four times their nodes: a long literal next to a product
/// would be copied into every suffix.
const MAX_SUFFIX_BYTES: usize = 64 * 1024;

fn suffix_product(nodes: &[&Node]) -> Option<Vec<Vec<u8>>> {
    if let [Node::Literal(bytes)] = nodes {
        return Some(vec![bytes.clone()]);
    }
    let mut parts: Vec<Vec<u8>> = vec![Vec::new()];
    // Σ (part + 1) bytes, and what the nodes themselves hold.
    let (mut size, mut source) = (1usize, 1usize);
    let fits = |size: usize, source: usize| size <= MAX_SUFFIX_BYTES.max(source.saturating_mul(4));
    for node in nodes {
        match node {
            Node::Literal(bytes) => {
                size = size.saturating_add(parts.len().saturating_mul(bytes.len()));
                source += bytes.len();
                if !fits(size, source) {
                    return None;
                }
                for p in parts.iter_mut() {
                    p.extend_from_slice(bytes);
                }
            }
            Node::Brace(branches) => {
                let mut lits = Vec::with_capacity(branches.len());
                for b in branches {
                    match b.seq() {
                        [] => lits.push(&b""[..]),
                        [Node::Literal(bytes)] => lits.push(bytes.as_slice()),
                        _ => return None,
                    }
                }
                if parts.len() > 1 && parts.len() * lits.len() > MAX_SUFFIX_PRODUCT {
                    return None;
                }
                let branch_bytes: usize = lits.iter().map(|l| l.len() + 1).sum();
                // Σ (|p| + |l| + 1) over every pair.
                size = (lits.len().saturating_mul(size - parts.len()))
                    .saturating_add(parts.len().saturating_mul(branch_bytes));
                source += branch_bytes;
                if !fits(size, source) {
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

/// Up to `LINEAR_SUFFIXES` the set is scanned in pattern order; past
/// it the suffixes are sorted by (length, bytes) and indexed per
/// length so `wild_consumes` can binary-search them.
fn affix_set(prefix: Bytes, suffixes: Vec<Vec<u8>>, ci: bool) -> WildKind {
    // The product's buffers are exact, so this keeps them as they are.
    let mut suffixes: Vec<Bytes> = suffixes.into_iter().map(Vec::into_boxed_slice).collect();
    let last = ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci);
    if suffixes.len() <= LINEAR_SUFFIXES {
        return WildKind::AffixSet(AffixSet {
            parts: std::iter::once(prefix).chain(suffixes).collect(),
            last,
        });
    }
    if ci {
        for s in &mut suffixes {
            s.make_ascii_lowercase();
        }
    }
    suffixes.sort_unstable_by(len_then_bytes);
    suffixes.dedup();
    let mut bytes = Vec::with_capacity(suffixes.iter().map(|s| s.len()).sum());
    let mut runs: Vec<SuffixRun> = Vec::new();
    for s in &suffixes {
        match runs.last_mut() {
            Some(run) if run.len as usize == s.len() => run.count += 1,
            _ => runs.push(SuffixRun {
                len: s.len() as u32,
                start: bytes.len() as u32,
                count: 1,
            }),
        }
        bytes.extend_from_slice(s);
    }
    WildKind::AffixIndex(Box::new(AffixIndex {
        prefix,
        suffixes: bytes.into_boxed_slice(),
        last,
        runs: runs.into_boxed_slice(),
    }))
}

/// (length, bytes) order. One comparator, so its sort is compiled once.
pub(super) fn len_then_bytes<T: AsRef<[u8]>>(a: &T, b: &T) -> std::cmp::Ordering {
    let (a, b) = (a.as_ref(), b.as_ref());
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

impl SegmentMatcher {
    /// The common part of a suffix set: the last-byte sketch, then
    /// the prefix.
    #[inline(always)]
    fn set_head(&self, prefix: &[u8], last: ByteSketch, seg: &[u8]) -> bool {
        last.may_end(seg)
            && seg.len() >= prefix.len()
            && self.affix_eq(prefix, &seg[..prefix.len()])
    }

    #[inline(always)]
    pub(super) fn wild_consumes(&self, w: &Wild, seg: &[u8]) -> bool {
        if w.dot_protect && seg.first() == Some(&b'.') {
            return false;
        }
        let len = seg.len();
        match &w.kind {
            WildKind::Affix(affix) => {
                let need = w.min_len as usize;
                if len < need || (!w.variable && len != need) {
                    return false;
                }
                let (prefix, suffix) = affix.parts();
                self.affix_eq(prefix, &seg[..prefix.len()])
                    && self.affix_eq(suffix, &seg[len - suffix.len()..])
            }
            WildKind::AffixSet(set) => {
                let [prefix, suffixes @ ..] = &set.parts[..] else {
                    unreachable!("an affix set has a prefix")
                };
                self.set_head(prefix, set.last, seg)
                    && suffixes.iter().any(|suf| {
                        let need = w.min_len as usize + suf.len();
                        len >= need
                            && (w.variable || len == need)
                            && self.affix_eq(suf, &seg[len - suf.len()..])
                    })
            }
            WildKind::AffixIndex(index) => {
                if !self.set_head(&index.prefix, index.last, seg) {
                    return false;
                }
                let Some(room) = len.checked_sub(w.min_len as usize) else {
                    return false;
                };
                index
                    .runs
                    .iter()
                    .take_while(|run| run.len as usize <= room)
                    .filter(|run| w.variable || run.len as usize == room)
                    .any(|run| self.run_has(index, run, &seg[len - run.len as usize..]))
            }
            WildKind::Generic(nfa) => nfa.matches(seg),
        }
    }

    /// Binary search of a run's suffixes for `tail`, in the branch-free
    /// shape `slice::binary_search_by` uses.
    fn run_has(&self, index: &AffixIndex, run: &SuffixRun, tail: &[u8]) -> bool {
        let len = run.len as usize;
        let order = |k: usize| {
            let s = &index.suffixes[run.start as usize + k * len..][..len];
            if self.case_insensitive {
                cmp_tail(s, tail)
            } else {
                s.cmp(tail)
            }
        };
        let (mut base, mut size) = (0usize, run.count as usize);
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            if order(mid) != Ordering::Greater {
                base = mid;
            }
            size -= half;
        }
        order(base) == Ordering::Equal
    }
}

/// Order of a lowercased suffix against a segment tail of the same
/// length, folding the tail's ASCII case.
fn cmp_tail(suffix: &[u8], tail: &[u8]) -> Ordering {
    suffix
        .iter()
        .zip(tail)
        .map(|(&a, &b)| a.cmp(&b.to_ascii_lowercase()))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}
