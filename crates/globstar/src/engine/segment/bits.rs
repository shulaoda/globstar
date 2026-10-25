//! Bit sets: the NFAs' state sets, a class as the bytes it takes, and a
//! 64-slot sketch of a set of bytes.

use crate::parse::ast::CharClass;

/// A state set: one `u64` up to 64 states, a boxed word slice beyond.
pub(super) trait Bits: Clone + PartialEq + std::fmt::Debug {
    fn zero(words: usize) -> Self;
    fn words(n_bits: usize) -> usize;
    fn set(&mut self, i: usize);
    fn test(&self, i: usize) -> bool;
    fn is_zero(&self) -> bool;
    fn clear(&mut self);
    fn or_with(&mut self, other: &Self);
    fn intersects(&self, other: &Self) -> bool;
    fn for_each(&self, f: impl FnMut(usize));
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
    fn intersects(&self, other: &Self) -> bool {
        *self & *other != 0
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
    fn intersects(&self, other: &Self) -> bool {
        self.0.iter().zip(other.0.iter()).any(|(a, b)| a & b != 0)
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

/// A set of bytes by `b % 64`: a byte outside the sketch is outside the set.
#[derive(Debug, Clone, Copy)]
pub(super) struct ByteSketch(u64);

impl ByteSketch {
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

/// A class as the bytes it takes, negation applied and separators left
/// out. `negated` stays for dot protection, where a negated class is a
/// wildcard.
#[derive(Debug, Clone)]
pub(super) struct ByteSet {
    pub(super) bits: [u64; 4],
    pub(super) negated: bool,
}

impl ByteSet {
    /// Case-insensitively a listed ASCII letter lists its other case too,
    /// before the negation (GLOB_SPEC §13).
    pub(super) fn of(cls: &CharClass, ci: bool) -> Self {
        let mut set = ByteSet {
            bits: [0; 4],
            negated: cls.negated,
        };
        for &(lo, hi) in &cls.ranges {
            for w in lo >> 6..=hi >> 6 {
                let from = if w == lo >> 6 { lo & 63 } else { 0 };
                let to = if w == hi >> 6 { hi & 63 } else { 63 };
                set.bits[usize::from(w)] |= (u64::MAX >> (63 - (to - from))) << from;
            }
        }
        if ci {
            for b in b'A'..=b'Z' {
                if set.has(b) || set.has(b | 0x20) {
                    set.add(b);
                    set.add(b | 0x20);
                }
            }
        }
        if cls.negated {
            set.bits = set.bits.map(|w| !w);
        }
        for &b in b"/\\" {
            if crate::engine::bytes::is_sep(b) {
                set.bits[usize::from(b >> 6)] &= !(1 << (b & 63));
            }
        }
        set
    }

    pub(super) fn add(&mut self, b: u8) {
        self.bits[usize::from(b >> 6)] |= 1 << (b & 63);
    }

    #[inline]
    pub(super) fn has(&self, b: u8) -> bool {
        self.bits[usize::from(b >> 6)] >> (b & 63) & 1 != 0
    }

    pub(super) fn is_empty(&self) -> bool {
        self.bits == [0; 4]
    }
}

#[cfg(test)]
mod tests {
    use super::ByteSet;
    use crate::engine::bytes::is_sep;
    use crate::parse::ast::{CharClass, Node};

    /// What the class takes, read off its ranges: never a separator.
    fn class_takes(cls: &CharClass, b: u8) -> bool {
        !is_sep(b) && (cls.ranges.iter().any(|&(lo, hi)| lo <= b && b <= hi) ^ cls.negated)
    }

    #[test]
    fn a_byte_set_takes_what_its_class_does() {
        for pattern in [
            "[a]",
            "[!a]",
            "[a-c]",
            "[!a-c]",
            "[]a]",
            "[]-]",
            "[a-]",
            "[\\]]",
            "[\\\\]",
            "[\x01-\x7f]",
            "[!\x01-\x7f]",
            "[\x1f-\x21]",
            "[?-A]",
            "[bbbbbb]",
            "[a-cb-dz]",
            "[!\u{e9}]",
            "[\u{e9}-\u{ff}]",
        ] {
            let ast = crate::parse::parse(pattern.as_bytes()).unwrap();
            let Node::Class(cls) = &ast.body else {
                panic!("{pattern} is not one class: {:?}", ast.body);
            };
            let set = ByteSet::of(cls, false);
            for b in 0..=255u8 {
                assert_eq!(
                    set.has(b),
                    class_takes(cls, b),
                    "{pattern} at byte {b:#04x}"
                );
            }
        }
    }
}
