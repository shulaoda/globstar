//! In-segment wildcard matchers: what a segment with a wildcard compiles
//! to (`compile_wild`) and how it takes a path segment
//! (`wild_consumes`). `wild_consumes` is forced inline (see `chain.rs`).

use crate::bytes::{affix_eq, fold};
use crate::parse::ast::Node;

use super::seg_nfa::SegNfa;
use super::{Bytes, SegmentMatcher};

// ── the wildcard matchers ─────────────────────────────────────────────────

/// A 64-slot sketch of a set of bytes (`b % 64`): a byte not in the
/// sketch is not in the set, so a set scan can be skipped.
#[derive(Debug, Clone, Copy)]
pub(super) struct ByteSketch(u64);

impl ByteSketch {
    /// Every byte: a string that ends with anything.
    pub(super) const ALL: ByteSketch = ByteSketch(u64::MAX);

    pub(super) fn of_last_bytes<'a>(strings: impl Iterator<Item = &'a [u8]>, ci: bool) -> Self {
        let mut bits = 0u64;
        for s in strings {
            match s.last() {
                // The empty string ends with anything.
                None => return ByteSketch::ALL,
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
            None => self.0 == ByteSketch::ALL.0,
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
    /// What the kind's tables take: a suffix set's bytes and bounds, an
    /// NFA's states and kept closures. The compile walk charges it
    /// (`GlobError::TooComplex`), so a brace pattern whose segments
    /// multiply cannot keep big tables in each of them.
    pub(super) fn table_bytes(&self) -> usize {
        match &self.kind {
            WildKind::Affix(_) => 0,
            WildKind::Suffixes(set) => set.suffixes.len() + set.bounds.len(),
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
    /// `lit (*|?)+ {lit,…}` (`*.{ts,tsx}`, a factored union of names):
    /// the prefix, then one suffix out of a set.
    Suffixes(Box<SuffixSet>),
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

/// A prefix and a set of suffixes, the suffixes grouped by their last
/// byte: a segment is compared with the few that end as it does,
/// whatever the set's size.
#[derive(Debug, Clone)]
pub(super) struct SuffixSet {
    prefix: Bytes,
    /// The last bytes of the suffixes: a chain's gate, and the short
    /// scan's.
    pub(super) last: ByteSketch,
    /// The non-empty suffixes laid end to end, grouped by last byte,
    /// each group in pattern order; suffix `k` is
    /// `suffixes[bounds[k]..bounds[k + 1]]`.
    suffixes: Bytes,
    bounds: Box<[u32]>,
    /// Past `LINEAR_SUFFIXES`: the group of last byte `b`
    /// (ASCII-lowercased when case-insensitive) is the suffixes
    /// `groups[b & GROUP_MASK]..groups[(b & GROUP_MASK) + 1]`; empty for
    /// fewer, which are scanned in turn.
    groups: Box<[u32]>,
    /// One suffix is empty: it ends anything.
    empty: bool,
}

/// A suffix set up to this size is scanned in turn after its sketch;
/// a larger one by the group of the segment's last byte: its low five
/// bits, which keep ASCII letters apart.
const LINEAR_SUFFIXES: usize = 4;
const GROUPS: usize = 32;
const GROUP_MASK: u8 = GROUPS as u8 - 1;

impl SuffixSet {
    fn new(prefix: Bytes, suffixes: Vec<Vec<u8>>, ci: bool) -> Self {
        let last = ByteSketch::of_last_bytes(suffixes.iter().map(|s| &s[..]), ci);
        let empty = suffixes.iter().any(Vec::is_empty);
        let group = |s: &[u8]| (fold(s[s.len() - 1], ci) & GROUP_MASK) as usize;
        let nonempty = || suffixes.iter().filter(|s| !s.is_empty());
        // Placed by a counting sort on the group, which keeps the
        // pattern's order within one: a pass counts each group's
        // suffixes and bytes, a second copies each suffix to its place.
        let mut groups = [0u32; GROUPS + 1];
        let mut at = [0u32; GROUPS + 1];
        for s in nonempty() {
            let g = group(s) + 1;
            groups[g] += 1;
            at[g] += s.len() as u32;
        }
        for g in 1..=GROUPS {
            groups[g] += groups[g - 1];
            at[g] += at[g - 1];
        }
        let mut bytes = vec![0u8; at[GROUPS] as usize];
        let mut bounds = vec![0u32; groups[GROUPS] as usize + 1];
        let mut next = groups;
        for s in nonempty() {
            let g = group(s);
            let (k, start) = (next[g] as usize, at[g] as usize);
            bytes[start..start + s.len()].copy_from_slice(s);
            bounds[k + 1] = (start + s.len()) as u32;
            next[g] += 1;
            at[g] += s.len() as u32;
        }
        let groups = if groups[GROUPS] as usize <= LINEAR_SUFFIXES {
            Box::default()
        } else {
            Box::from(&groups[..])
        };
        SuffixSet {
            prefix,
            last,
            suffixes: bytes.into_boxed_slice(),
            bounds: bounds.into_boxed_slice(),
            groups,
            empty,
        }
    }

    /// Does a suffix of at most `room` bytes end `seg` (`variable`), or
    /// one of exactly `room` bytes? A few suffixes are compared in turn
    /// after the sketch; of many, only those that end as `seg` does.
    /// Out of line: inlined, it weighed on every other wildcard kind.
    #[inline(never)]
    fn ends(&self, seg: &[u8], room: usize, variable: bool, ci: bool) -> bool {
        let Some(&last) = seg.last() else {
            return false;
        };
        let bounds = if self.groups.is_empty() {
            if !self.last.may_end(seg) {
                return false;
            }
            &self.bounds[..]
        } else {
            let g = (fold(last, ci) & GROUP_MASK) as usize;
            &self.bounds[self.groups[g] as usize..=self.groups[g + 1] as usize]
        };
        for k in 1..bounds.len() {
            let suffix = &self.suffixes[bounds[k - 1] as usize..bounds[k] as usize];
            if suffix.len() <= room
                && (variable || suffix.len() == room)
                && affix_eq(suffix, &seg[seg.len() - suffix.len()..], ci)
            {
                return true;
            }
        }
        false
    }
}

// ── compiling ─────────────────────────────────────────────────────────────

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
        WildKind::Suffixes(Box::new(SuffixSet::new(prefix, suffixes, ci)))
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

// ── matching ──────────────────────────────────────────────────────────────

impl SegmentMatcher {
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
            WildKind::Suffixes(set) => {
                // The prefix, then the suffixes that end as the segment
                // does, over what the prefix and the `?`s leave.
                let Some(room) = len.checked_sub(w.min_len as usize) else {
                    return false;
                };
                if !self.affix_eq(&set.prefix, &seg[..set.prefix.len()]) {
                    return false;
                }
                (set.empty && (w.variable || room == 0))
                    || set.ends(seg, room, w.variable, self.case_insensitive)
            }
            WildKind::Generic(nfa) => nfa.matches(seg),
        }
    }
}
