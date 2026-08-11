//! Segment-at-a-time matching over compiled [`ElemSeq`]s.

use super::{Elem, ElemSeq, SegmentMatcher, Wild, WildKind, is_sep};

/// Iterator over `(start, end)` byte ranges of a path's segments.
/// Splits on every `Seps` byte; the empty path yields one empty
/// segment.
#[derive(Clone, Copy)]
struct SegIter<'a> {
    path: &'a [u8],
    pos: usize,
}

impl<'a> SegIter<'a> {
    fn new(path: &'a [u8]) -> Self {
        Self { path, pos: 0 }
    }
}

impl<'a> Iterator for SegIter<'a> {
    type Item = (usize, usize);

    #[inline]
    fn next(&mut self) -> Option<(usize, usize)> {
        let start = self.pos;
        if start > self.path.len() {
            return None;
        }
        let mut i = start;
        while i < self.path.len() && !is_sep(self.path[i]) {
            i += 1;
        }
        // Advance unconditionally: after the final segment `pos` is
        // `len + 1`, a sentinel no real segment start can equal.
        // Exhaustion and the single-globstar overlap check both read it.
        self.pos = i + 1;
        Some((start, i))
    }
}

#[inline]
fn accept_bit(seq: &ElemSeq) -> u64 {
    1u64 << (seq.num_states - 1)
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

impl SegmentMatcher {
    #[inline(always)]
    pub(super) fn seq_matches(&self, seq: &ElemSeq, path: &[u8]) -> bool {
        match seq.g_count {
            0 => self.match_fixed(seq, path),
            1 => self.match_single_g(seq, path),
            _ => self.nfa_run(seq, path) & accept_bit(seq) != 0,
        }
    }

    /// Fixed-depth: element count must equal segment count; positional
    /// compare.
    fn match_fixed(&self, seq: &ElemSeq, path: &[u8]) -> bool {
        let mut segs = SegIter::new(path);
        for e in seq.elems.iter() {
            let Some((s, t)) = segs.next() else {
                return false;
            };
            if !self.elem_consumes(e, &path[s..t]) {
                return false;
            }
        }
        segs.next().is_none()
    }

    /// Single-globstar: anchored tail (checked first — it rejects
    /// fastest), anchored head, globstar absorbs the middle. No
    /// searching, and at most one pass over the head + tail byte ranges.
    #[inline(always)]
    fn match_single_g(&self, seq: &ElemSeq, path: &[u8]) -> bool {
        let g = seq.single_g as usize;
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
            if !self.elem_consumes(&seq.elems[g + 1 + j], &path[s..tail_end]) {
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
        let mid_start = if !seq.joined_head.is_empty() {
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
                    self.eq_byte(hb, pb)
                };
                if !ok {
                    return false;
                }
            }
            head.len()
        } else {
            let mut iter = SegIter::new(path);
            for e in seq.elems[..g].iter() {
                let Some((s, t)) = iter.next() else {
                    return false;
                };
                if !self.elem_consumes(e, &path[s..t]) {
                    return false;
                }
            }
            // Start of segment g — `len + 1` sentinel when the path ran
            // out of segments (see `SegIter::next`), which fails the
            // overlap check below whenever the tail (or G1) still needs
            // one.
            iter.pos
        };

        // Overlap / arity check and the absorbed middle's byte range.
        let (mid_exists, mid_end) = if tail_len > 0 {
            if ts < mid_start {
                return false; // head and tail would share segments
            }
            (ts > mid_start, ts.saturating_sub(1))
        } else {
            (mid_start <= path.len(), path.len())
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
        if self.dot || !mid_exists {
            return true;
        }
        // `mid_exists` already implies `mid_start <= mid_end` in both arms above.
        !has_dot_led_segment(path, mid_start, mid_end)
    }

    /// General element-NFA run (multi-globstar `is_match` and every
    /// `match_dir`). Returns the active state mask after consuming all
    /// segments.
    fn nfa_run(&self, seq: &ElemSeq, path: &[u8]) -> u64 {
        let mut active = seq.eps[seq.state_of[0] as usize];
        for (s, t) in SegIter::new(path) {
            if active == 0 {
                return 0;
            }
            active = self.nfa_step(seq, active, &path[s..t]);
        }
        active
    }

    /// One segment step of the element NFA.
    fn nfa_step(&self, seq: &ElemSeq, active: u64, seg: &[u8]) -> u64 {
        let mut next: u64 = 0;
        let m = seq.elems.len();
        let seg_dot_led = !seg.is_empty() && seg[0] == b'.';
        let absorb_ok = self.dot || !seg_dot_led;
        let mut bits = active;
        while bits != 0 {
            let s = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            if s as u8 == seq.num_states - 1 {
                continue; // accept has no outgoing transitions
            }
            let i = seq.elem_of[s] as usize;
            let entry = seq.state_of[i] as usize;
            let next_entry = if i + 1 < m {
                seq.state_of[i + 1] as usize
            } else {
                (seq.num_states - 1) as usize
            };
            match &seq.elems[i] {
                Elem::Lit(lit) => {
                    if self.lit_eq(lit, seg) {
                        next |= seq.eps[next_entry];
                    }
                }
                Elem::Wild(w) => {
                    if self.wild_consumes(w, seg) {
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
    pub(super) fn seq_match_dir(&self, seq: &ElemSeq, dir: &[u8]) -> (bool, bool) {
        let active = self.nfa_run(seq, dir);
        let exact = active & accept_bit(seq) != 0;
        let prefix = active & seq.reach1 != 0;
        (exact, prefix)
    }

    // ---------------------------------------------------------------------------
    // Element / wild consumption (dot protection is baked in at compile)
    // ---------------------------------------------------------------------------

    #[inline(always)]
    fn elem_consumes(&self, e: &Elem, seg: &[u8]) -> bool {
        match e {
            Elem::Lit(lit) => self.lit_eq(lit, seg),
            Elem::Wild(w) => self.wild_consumes(w, seg),
            _ => false, // globstars are never positionally consumed
        }
    }

    #[inline]
    fn lit_eq(&self, lit: &[u8], seg: &[u8]) -> bool {
        if !self.case_insensitive {
            return lit == seg;
        }
        lit.len() == seg.len()
            && lit
                .iter()
                .zip(seg)
                .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
    }

    #[inline]
    pub(super) fn affix_eq(&self, part: &[u8], seg_part: &[u8]) -> bool {
        debug_assert_eq!(part.len(), seg_part.len());
        if !self.case_insensitive {
            return part == seg_part;
        }
        part.iter()
            .zip(seg_part)
            .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
    }

    #[inline(always)]
    fn wild_consumes(&self, w: &Wild, seg: &[u8]) -> bool {
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
                self.affix_eq(prefix, &seg[..prefix.len()])
                    && self.affix_eq(suffix, &seg[len - suffix.len()..])
            }
            WildKind::AffixSet { prefix, suffixes } => {
                if len < prefix.len() || !self.affix_eq(prefix, &seg[..prefix.len()]) {
                    return false;
                }
                suffixes.iter().any(|suf| {
                    let need = w.min_len as usize + suf.len();
                    len >= need
                        && (w.variable || len == need)
                        && self.affix_eq(suf, &seg[len - suf.len()..])
                })
            }
            WildKind::Generic(nfa) => nfa.matches(seg),
        }
    }

    fn eq_byte(&self, a: u8, b: u8) -> bool {
        if self.case_insensitive {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    }
}
