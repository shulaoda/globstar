//! Bit sets for the NFA simulations: one `u64` when the automaton has
//! at most 64 states, a boxed word slice beyond. Both NFAs (the graph
//! `Nfa` and the in-segment `SegNfa`) are generic over this trait so
//! the common case is a plain register.

pub(super) trait Bits: Clone + PartialEq + std::fmt::Debug {
    /// The empty set over `words` words.
    fn zero(words: usize) -> Self;
    /// Words needed for `n_bits` bits.
    fn words(n_bits: usize) -> usize;
    fn set(&mut self, i: usize);
    fn test(&self, i: usize) -> bool;
    fn is_zero(&self) -> bool;
    fn clear(&mut self);
    fn or_with(&mut self, other: &Self);
    fn and_with(&mut self, other: &Self);
    fn intersects(&self, other: &Self) -> bool;
    /// How many bits are set.
    fn count(&self) -> usize;
    /// Visit every set bit, ascending.
    fn for_each(&self, f: impl FnMut(usize));
    /// The words, lowest bits first.
    fn words_mut(&mut self) -> &mut [u64];
}

impl Bits for u64 {
    #[inline]
    fn zero(_: usize) -> Self {
        0
    }
    #[inline]
    fn words(_: usize) -> usize {
        1
    }
    #[inline]
    fn set(&mut self, i: usize) {
        *self |= 1u64 << i;
    }
    #[inline]
    fn test(&self, i: usize) -> bool {
        *self & (1u64 << i) != 0
    }
    #[inline]
    fn is_zero(&self) -> bool {
        *self == 0
    }
    #[inline]
    fn clear(&mut self) {
        *self = 0;
    }
    #[inline]
    fn or_with(&mut self, other: &Self) {
        *self |= *other;
    }
    #[inline]
    fn and_with(&mut self, other: &Self) {
        *self &= *other;
    }
    #[inline]
    fn intersects(&self, other: &Self) -> bool {
        *self & *other != 0
    }
    #[inline]
    fn count(&self) -> usize {
        self.count_ones() as usize
    }
    #[inline]
    fn for_each(&self, mut f: impl FnMut(usize)) {
        let mut bits = *self;
        while bits != 0 {
            f(bits.trailing_zeros() as usize);
            bits &= bits - 1;
        }
    }
    #[inline]
    fn words_mut(&mut self) -> &mut [u64] {
        std::slice::from_mut(self)
    }
}

/// A set over more than 64 states.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Wide(Box<[u64]>);

impl Wide {
    #[inline]
    pub(super) fn unset(&mut self, i: usize) {
        self.0[i >> 6] &= !(1u64 << (i & 63));
    }
}

impl Bits for Wide {
    fn zero(words: usize) -> Self {
        Wide(vec![0u64; words].into_boxed_slice())
    }
    fn words(n_bits: usize) -> usize {
        n_bits.div_ceil(64).max(1)
    }
    #[inline]
    fn set(&mut self, i: usize) {
        self.0[i >> 6] |= 1u64 << (i & 63);
    }
    #[inline]
    fn test(&self, i: usize) -> bool {
        self.0[i >> 6] & (1u64 << (i & 63)) != 0
    }
    fn is_zero(&self) -> bool {
        self.0.iter().all(|&w| w == 0)
    }
    fn clear(&mut self) {
        self.0.fill(0);
    }
    fn or_with(&mut self, other: &Self) {
        for (a, b) in self.0.iter_mut().zip(other.0.iter()) {
            *a |= *b;
        }
    }
    fn and_with(&mut self, other: &Self) {
        for (a, b) in self.0.iter_mut().zip(other.0.iter()) {
            *a &= *b;
        }
    }
    fn intersects(&self, other: &Self) -> bool {
        self.0.iter().zip(other.0.iter()).any(|(a, b)| a & b != 0)
    }
    fn count(&self) -> usize {
        self.0.iter().map(|w| w.count_ones() as usize).sum()
    }
    fn for_each(&self, mut f: impl FnMut(usize)) {
        for (w, &word) in self.0.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                f(w * 64 + bits.trailing_zeros() as usize);
                bits &= bits - 1;
            }
        }
    }
    #[inline]
    fn words_mut(&mut self) -> &mut [u64] {
        &mut self.0
    }
}
