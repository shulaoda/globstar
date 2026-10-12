use crate::dir_match::DirMatch;

use super::bits::Bits;
use super::compile::lit_cmp;
use super::{ByteSketch, Chain, Elem, LitSet, MANY_G, NO_G, Nfa, SegmentMatcher, Wild, WildKind};

/// Index just past the segment that starts at `pos`.
#[inline(always)]
fn seg_end(path: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i < path.len() && !std::path::is_separator(path[i] as char) {
        i += 1;
    }
    i
}

/// Start of the segment that ends at `end`.
#[inline(always)]
fn seg_start(path: &[u8], end: usize) -> usize {
    let mut s = end;
    while s > 0 && !std::path::is_separator(path[s - 1] as char) {
        s -= 1;
    }
    s
}

/// A chain's elements in order: the matcher's slice for the lone path
/// of a brace-free pattern, a node list over it otherwise.
trait ChainElems {
    fn len(&self) -> usize;
    fn at(&self, i: usize) -> &Elem;
}

impl ChainElems for &[Elem] {
    #[inline(always)]
    fn len(&self) -> usize {
        <[Elem]>::len(self)
    }
    #[inline(always)]
    fn at(&self, i: usize) -> &Elem {
        &self[i]
    }
}

struct Mapped<'a> {
    nodes: &'a [u32],
    elems: &'a [Elem],
}

impl ChainElems for Mapped<'_> {
    #[inline(always)]
    fn len(&self) -> usize {
        self.nodes.len()
    }
    #[inline(always)]
    fn at(&self, i: usize) -> &Elem {
        &self.elems[self.nodes[i] as usize]
    }
}

impl SegmentMatcher {
    /// The suffix prefilter: `false` means the path cannot match.
    #[inline(always)]
    pub(super) fn prefilter(&self, path: &[u8]) -> bool {
        match &self.suffixes[..] {
            [] => true,
            [one] => self.ends_with(path, one),
            set => self.suffix_last.may_end(path) && set.iter().any(|s| self.ends_with(path, s)),
        }
    }

    /// A pattern byte against a path byte: `/` stands for any
    /// separator, letters fold when case-insensitive.
    #[inline(always)]
    fn pat_byte_eq(&self, pat: u8, b: u8) -> bool {
        if pat == b'/' {
            std::path::is_separator(b as char)
        } else if self.case_insensitive {
            pat.eq_ignore_ascii_case(&b)
        } else {
            pat == b
        }
    }

    /// Suffix test, last byte first: that is where extensions differ.
    #[inline(always)]
    fn ends_with(&self, path: &[u8], suffix: &[u8]) -> bool {
        path.len() >= suffix.len()
            && suffix
                .iter()
                .zip(&path[path.len() - suffix.len()..])
                .rev()
                .all(|(&s, &p)| self.pat_byte_eq(s, p))
    }

    /// Does the path end like the chain's last element (a literal, or
    /// an affix's suffix)? Rejects most members of a union before the
    /// chain runs; a single chain has the prefilter for that.
    #[inline(always)]
    pub(super) fn ends_like(&self, i: usize, path: &[u8]) -> bool {
        let chain = &self.chains[i];
        let last = chain
            .nodes
            .last()
            .map_or(self.elems.len() - 1, |&n| n as usize);
        let suffix: &[u8] = match &self.elems[last] {
            Elem::Lit(lit) => lit,
            Elem::Wild(Wild {
                kind: WildKind::Affix { suffix, .. },
                ..
            }) => suffix,
            _ => &[],
        };
        path.len() >= suffix.len() && self.affix_eq(suffix, &path[path.len() - suffix.len()..])
    }

    #[inline(always)]
    pub(super) fn chain_matches(&self, i: usize, path: &[u8]) -> bool {
        let chain = &self.chains[i];
        if chain.nodes.is_empty() {
            self.chain_matches_on(chain, &&self.elems[..], path)
        } else {
            let elems = Mapped {
                nodes: &chain.nodes,
                elems: &self.elems,
            };
            self.chain_matches_on(chain, &elems, path)
        }
    }

    #[inline(always)]
    fn chain_matches_on<E: ChainElems>(&self, chain: &Chain, elems: &E, path: &[u8]) -> bool {
        match chain.globstar {
            NO_G => self.fixed_run(elems, 0..elems.len(), path, 0),
            MANY_G => match &chain.nfa {
                Some(nfa) => nfa.is_match(self, path),
                // The lone path of a linear pattern: the matcher's NFA
                // is its own.
                None => self
                    .nfa
                    .as_ref()
                    .is_some_and(|nfa| nfa.is_match(self, path)),
            },
            g => {
                let g = g as usize;
                match self.head(elems, g, path) {
                    Some(mid_start) => self.tail_and_globstar(elems, g, path, mid_start),
                    None => false,
                }
            }
        }
    }

    /// `elems[i]` against the segment at `pos`: `Some(end)` is the index
    /// just past it. A literal is compared in place and must end at a
    /// separator or the end; a wildcard needs the segment's end first.
    #[inline(always)]
    fn consume_at<E: ChainElems>(
        &self,
        elems: &E,
        i: usize,
        path: &[u8],
        pos: usize,
    ) -> Option<usize> {
        match elems.at(i) {
            Elem::Lit(lit) => {
                let end = pos + lit.len();
                let bounded = end == path.len()
                    || (end < path.len() && std::path::is_separator(path[end] as char));
                (bounded && self.affix_eq(lit, &path[pos..end])).then_some(end)
            }
            Elem::Wild(w) => {
                let end = seg_end(path, pos);
                self.wild_consumes(w, &path[pos..end]).then_some(end)
            }
            _ => None,
        }
    }

    /// Do `elems[range]` take the segments from `pos` to the end, one
    /// each?
    fn fixed_run<E: ChainElems>(
        &self,
        elems: &E,
        range: std::ops::Range<usize>,
        path: &[u8],
        mut pos: usize,
    ) -> bool {
        let last = range.end - 1;
        for i in range.start..last {
            match self.consume_at(elems, i, path, pos) {
                Some(end) if end < path.len() => pos = end + 1,
                _ => return false,
            }
        }
        self.consume_at(elems, last, path, pos) == Some(path.len())
    }

    /// The head `elems[..g]`, one segment each: where the segment after
    /// it starts. Every globstar wants that segment (`G1` absorbs one,
    /// `G0` is followed by its tail), so a path that ends in the head
    /// fails.
    #[inline(always)]
    fn head<E: ChainElems>(&self, elems: &E, g: usize, path: &[u8]) -> Option<usize> {
        let mut pos = 0;
        for i in 0..g {
            match self.consume_at(elems, i, path, pos) {
                Some(end) if end < path.len() => pos = end + 1,
                _ => return None,
            }
        }
        Some(pos)
    }

    /// With the head consumed up to `mid_start`: the tail
    /// `elems[g + 1..]` on the last segments, found backward, and the
    /// globstar on what lies between.
    fn tail_and_globstar<E: ChainElems>(
        &self,
        elems: &E,
        g: usize,
        path: &[u8],
        mid_start: usize,
    ) -> bool {
        let n = elems.len();
        // Where the tail starts; past the end when there is none.
        let mut split = path.len() + 1;
        let mut end = path.len();
        for j in (g + 1..n).rev() {
            let s = seg_start(path, end);
            if s < mid_start || !self.elem_consumes(elems.at(j), &path[s..end]) {
                return false;
            }
            split = s;
            if j > g + 1 {
                if s == 0 {
                    return false;
                }
                end = s - 1;
            }
        }
        self.absorbs(elems.at(g), path, mid_start, split)
    }

    /// Can the globstar `kind` take the segments between the head
    /// (ending before `from`) and the tail starting at `split`?
    /// `split == from` means no segment at all.
    fn absorbs(&self, kind: &Elem, path: &[u8], from: usize, split: usize) -> bool {
        if split == from {
            return kind.nullable();
        }
        // Up to the separator before the tail, or the end.
        let to = split - 1;
        // `G0Strict` may not start with an empty segment.
        if matches!(kind, Elem::G0Strict)
            && (from == to || std::path::is_separator(path[from] as char))
        {
            return false;
        }
        self.dot_free(path, from, to)
    }

    /// Under `dot=false`, no segment within `path[from..to)` may start
    /// with a `.`.
    fn dot_free(&self, path: &[u8], from: usize, to: usize) -> bool {
        if self.dot {
            return true;
        }
        if from < to && path[from] == b'.' {
            return false;
        }
        !(from..to)
            .any(|i| std::path::is_separator(path[i] as char) && i + 1 < to && path[i + 1] == b'.')
    }

    /// `match_dir` through the lone chain of a matcher that keeps no
    /// NFA.
    pub(super) fn chain_dir(&self, dir: &[u8]) -> DirMatch {
        let chain = &self.chains[0];
        if chain.nodes.is_empty() {
            self.chain_dir_on(chain.globstar, &&self.elems[..], dir)
        } else {
            let elems = Mapped {
                nodes: &chain.nodes,
                elems: &self.elems,
            };
            self.chain_dir_on(chain.globstar, &elems, dir)
        }
    }

    /// The tokens the NFA would hold after `dir`, read off the chain: a
    /// head position when the directory ends inside the head, the
    /// globstar when it has taken everything after the head, and a
    /// tail position when the tail's first elements take the last
    /// segments.
    fn chain_dir_on<E: ChainElems>(&self, globstar: u32, elems: &E, dir: &[u8]) -> DirMatch {
        let n = elems.len();
        let g = if globstar == NO_G {
            n
        } else {
            globstar as usize
        };
        let tl = n - g - usize::from(g < n);
        // Every element from `i` on takes some segment: a token there
        // still reaches accept.
        let open = |i: usize| (i..n).all(|j| elems.at(j).satisfiable());
        let tail_any = (n - tl..n).all(|j| elems.at(j).any_seg());
        if dir.is_empty() {
            // The walk root: nothing consumed yet. All below iff a
            // leading globstar is followed by at most one any-segment.
            let all = self.dot && g == 0 && tl <= 1 && tail_any;
            return DirMatch::from_exact_prefix_all(self.is_match(dir), true, all);
        }
        let mut mid_start = 0;
        for i in 0..g {
            let Some(end) = self.consume_at(elems, i, dir, mid_start) else {
                return DirMatch::Pruned;
            };
            if end == dir.len() {
                // `dir` is the chain's first `k` segments. A globstar
                // reached with nothing absorbed covers all below iff its
                // tail is at most one any-segment.
                let k = i + 1;
                let all = self.dot && g < n && k == g && tl <= 1 && tail_any;
                return DirMatch::from_exact_prefix_all(k == n, k < n && open(k), all);
            }
            mid_start = end + 1;
        }
        if g == n {
            // A fixed chain, and the directory is deeper.
            return DirMatch::Pruned;
        }
        let kind = elems.at(g);
        let exact = self.tail_and_globstar(elems, g, dir, mid_start);
        // The globstar took everything after the head.
        let g_alive = self.absorbs(kind, dir, mid_start, dir.len() + 1);
        let mut prefix = g_alive && open(g + 1);
        let mut all = self.dot && g_alive && tail_any;
        // Tail tokens: `elems[g + 1..g + 1 + j]` on the last `j`
        // segments with the globstar released before them. A dead
        // globstar may leave one; `all` needs them all.
        if (!prefix || all) && tl > 1 {
            let mut end = dir.len();
            for j in 1..tl {
                let start = seg_start(dir, end);
                let alive = self.absorbs(kind, dir, mid_start, start)
                    && self.fixed_run(elems, g + 1..g + 1 + j, dir, start);
                prefix |= alive && open(g + 1 + j);
                all &= alive;
                if start == mid_start {
                    // No segment is left for a longer tail.
                    all &= j + 1 == tl;
                    break;
                }
                end = start - 1;
            }
        }
        DirMatch::from_exact_prefix_all(exact, prefix, all)
    }

    #[inline(always)]
    fn elem_consumes(&self, e: &Elem, seg: &[u8]) -> bool {
        match e {
            Elem::Lit(lit) => self.lit_eq(lit, seg),
            Elem::Wild(w) => self.wild_consumes(w, seg),
            _ => false,
        }
    }

    #[inline]
    fn lit_eq(&self, lit: &[u8], seg: &[u8]) -> bool {
        lit.len() == seg.len() && self.affix_eq(lit, seg)
    }

    /// Byte-wise, so short affixes stay inline rather than calling
    /// `bcmp`.
    #[inline]
    fn affix_eq(&self, part: &[u8], seg_part: &[u8]) -> bool {
        debug_assert_eq!(part.len(), seg_part.len());
        if self.case_insensitive {
            part.iter()
                .zip(seg_part)
                .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
        } else {
            part.iter().zip(seg_part).all(|(a, b)| a == b)
        }
    }

    /// The common part of a suffix set: the last-byte sketch, then
    /// the prefix.
    #[inline(always)]
    fn set_head(&self, prefix: &[u8], last: ByteSketch, seg: &[u8]) -> bool {
        last.may_end(seg)
            && seg.len() >= prefix.len()
            && self.affix_eq(prefix, &seg[..prefix.len()])
    }

    #[inline(always)]
    fn wild_consumes(&self, w: &Wild, seg: &[u8]) -> bool {
        if w.dot_protect && seg.first() == Some(&b'.') {
            return false;
        }
        let len = seg.len();
        match &w.kind {
            WildKind::Affix { prefix, suffix } => {
                let need = w.min_len as usize;
                if len < need || (!w.variable && len != need) {
                    return false;
                }
                self.affix_eq(prefix, &seg[..prefix.len()])
                    && self.affix_eq(suffix, &seg[len - suffix.len()..])
            }
            WildKind::AffixSet {
                prefix,
                suffixes,
                last,
            } => {
                self.set_head(prefix, *last, seg)
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
                    .any(|run| {
                        let tail = &seg[len - run.len as usize..];
                        let set = &index.suffixes[run.start as usize..run.end as usize];
                        if self.case_insensitive {
                            set.binary_search_by(|s| cmp_folded(s, tail)).is_ok()
                        } else {
                            set.binary_search_by(|s| s[..].cmp(tail)).is_ok()
                        }
                    })
            }
            WildKind::Generic(nfa) => nfa.matches(seg),
        }
    }
}

/// Order of a lowercased suffix against a segment tail of the same
/// length, folding the tail's ASCII case.
fn cmp_folded(suffix: &[u8], tail: &[u8]) -> std::cmp::Ordering {
    suffix
        .iter()
        .zip(tail)
        .map(|(&a, &b)| a.cmp(&b.to_ascii_lowercase()))
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

impl<B: Bits> Nfa<B> {
    #[inline(always)]
    fn accept(&self) -> usize {
        self.states.len() - 1
    }

    pub(super) fn is_match(&self, m: &SegmentMatcher, path: &[u8]) -> bool {
        self.run(m, path).test(self.accept())
    }

    pub(super) fn match_dir(&self, m: &SegmentMatcher, dir_path: &[u8]) -> DirMatch {
        // The empty dir is the walk root: nothing consumed yet.
        let root = dir_path.is_empty();
        let (mut active, exact) = if root {
            (self.init.clone(), m.is_match(dir_path))
        } else {
            let active = self.run(m, dir_path);
            let exact = active.test(self.accept());
            (active, exact)
        };
        let prefix = root || active.intersects(&self.reach1);
        // `dot=false` never covers a subtree: no wildcard matches the
        // dot-led names below.
        let all = m.dot && prefix && self.covers_below(m, &mut active);
        DirMatch::from_exact_prefix_all(exact, prefix, all)
    }

    /// The active set after every segment of `path`.
    fn run(&self, m: &SegmentMatcher, path: &[u8]) -> B {
        let mut active = self.init.clone();
        let mut next = B::zero(B::words(self.states.len()));
        let mut pos = 0;
        while !active.is_zero() {
            let end = seg_end(path, pos);
            self.step(m, &active, &path[pos..end], &mut next);
            std::mem::swap(&mut active, &mut next);
            if end == path.len() {
                break;
            }
            pos = end + 1;
        }
        active
    }

    fn step(&self, m: &SegmentMatcher, active: &B, seg: &[u8], next: &mut B) {
        next.clear();
        let accept = self.accept();
        let absorb_ok = m.dot || seg.first() != Some(&b'.');
        // Literal siblings: one search per set instead of one compare
        // per state; their states are skipped below.
        let grouped = self.lit_sets.as_deref().map(|lit_sets| {
            for set in lit_sets.sets.iter() {
                if active.intersects(&set.states) {
                    self.fire_lit_set(m, set, active, seg, next);
                }
            }
            &lit_sets.states
        });
        active.for_each(|s| {
            if s == accept || grouped.is_some_and(|g| g.test(s)) {
                return;
            }
            let state = &self.states[s];
            let node = state.node as usize;
            let consumes = match &m.elems[node] {
                Elem::Lit(lit) => m.lit_eq(lit, seg),
                Elem::Wild(w) => m.wild_consumes(w, seg),
                Elem::G0 | Elem::G1 => absorb_ok,
                // The entry state is the first of the node's two and
                // may not take an empty segment.
                Elem::G0Strict => {
                    let entry = s == 0 || self.states[s - 1].node as usize != node;
                    absorb_ok && !(entry && seg.is_empty())
                }
            };
            if consumes {
                next.or_with(&state.after);
            }
        });
    }

    /// Fire the set's active states whose literal is `seg`: equal
    /// literals sit together in `sorted`, one binary search away.
    fn fire_lit_set(
        &self,
        m: &SegmentMatcher,
        set: &LitSet<B>,
        active: &B,
        seg: &[u8],
        next: &mut B,
    ) {
        let equal = |s: u32| {
            let lit = m.elems[self.states[s as usize].node as usize].as_lit();
            lit_cmp(lit, seg, m.case_insensitive)
        };
        let Ok(mut k) = set.sorted.binary_search_by(|&s| equal(s)) else {
            return;
        };
        while k > 0 && equal(set.sorted[k - 1]).is_eq() {
            k -= 1;
        }
        for &s in set.sorted[k..].iter().take_while(|&&s| equal(s).is_eq()) {
            if active.test(s as usize) {
                next.or_with(&self.states[s as usize].after);
            }
        }
    }

    /// `step` over a non-empty segment that only wildcards consume:
    /// every literal and class rejects it (`dot=true` only).
    fn step_wild(&self, m: &SegmentMatcher, active: &B, next: &mut B) {
        next.clear();
        let accept = self.accept();
        active.for_each(|s| {
            if s == accept {
                return;
            }
            let state = &self.states[s];
            if m.elems[state.node as usize].any_seg() {
                next.or_with(&state.after);
            }
        });
    }

    /// Does every path below the directory match? `active` is the
    /// state set the directory's segments leave behind.
    ///
    /// Feeds wildcard-only segments until the set settles, requiring
    /// an accept after each one. A real segment fires every
    /// transition a wildcard-only one does, so `true` holds for all
    /// real paths; `false` may only mean "not provable".
    fn covers_below(&self, m: &SegmentMatcher, active: &mut B) -> bool {
        let mut next = B::zero(B::words(self.states.len()));
        // Tokens only move forward or sit on a globstar, so the set
        // settles within the number of states.
        for _ in 0..=self.states.len() {
            self.step_wild(m, active, &mut next);
            if !next.test(self.accept()) {
                return false;
            }
            if next == *active {
                return true;
            }
            std::mem::swap(active, &mut next);
        }
        false
    }
}
