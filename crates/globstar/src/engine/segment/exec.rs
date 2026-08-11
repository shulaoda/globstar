use super::{Elem, ElemSeq, SegmentMatcher, Wild, WildKind};

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
        while i < self.path.len() && !std::path::is_separator(self.path[i] as char) {
            i += 1;
        }
        self.pos = i + 1;
        Some((start, i))
    }
}

impl SegmentMatcher {
    #[inline(always)]
    pub(super) fn seq_matches(&self, seq: &ElemSeq, path: &[u8]) -> bool {
        match seq.g_count {
            0 => self.match_fixed(seq, path),
            1 => self.match_single_g(seq, path),
            _ => self.nfa_run(seq, path) & (1u64 << (seq.num_states - 1)) != 0,
        }
    }

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

    fn match_single_g(&self, seq: &ElemSeq, path: &[u8]) -> bool {
        let g = seq.single_g as usize;
        let m = seq.elems.len();
        let tail_len = m - g - 1;

        let mut tail_end = path.len();
        let mut ts = 0usize;
        for j in (0..tail_len).rev() {
            let mut s = tail_end;
            while s > 0 && !std::path::is_separator(path[s - 1] as char) {
                s -= 1;
            }
            if !self.elem_consumes(&seq.elems[g + 1 + j], &path[s..tail_end]) {
                return false;
            }
            if j > 0 {
                if s == 0 {
                    return false;
                }
                tail_end = s - 1;
            }
            ts = s;
        }

        let mid_start = if !seq.joined_head.is_empty() {
            let head = &seq.joined_head;
            if path.len() < head.len() {
                return false;
            }
            for (i, &hb) in head.iter().enumerate() {
                let pb = path[i];
                let ok = if hb == b'/' {
                    std::path::is_separator(pb as char)
                } else {
                    if self.case_insensitive {
                        hb.eq_ignore_ascii_case(&pb)
                    } else {
                        hb == pb
                    }
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
            iter.pos
        };

        let (mid_exists, mid_end) = if tail_len > 0 {
            if ts < mid_start {
                return false;
            }
            (ts > mid_start, ts.saturating_sub(1))
        } else {
            (mid_start <= path.len(), path.len())
        };

        match seq.elems[g] {
            Elem::G0 => {}
            Elem::G0Strict => {
                if mid_exists
                    && (mid_start >= path.len() || std::path::is_separator(path[mid_start] as char))
                {
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

        if self.dot || !mid_exists {
            return true;
        }
        if mid_start < mid_end && path[mid_start] == b'.' {
            return false;
        }
        let mut i = mid_start;
        while i < mid_end {
            if std::path::is_separator(path[i] as char) && i + 1 < mid_end && path[i + 1] == b'.' {
                return false;
            }
            i += 1;
        }
        true
    }

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
                continue;
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
                    if absorb_ok && !(s == entry && seg.is_empty()) {
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

    pub(super) fn seq_match_dir(&self, seq: &ElemSeq, dir: &[u8]) -> (bool, bool) {
        let active = self.nfa_run(seq, dir);
        let exact = active & (1u64 << (seq.num_states - 1)) != 0;
        let prefix = active & seq.reach1 != 0;
        (exact, prefix)
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
}
