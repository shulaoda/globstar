// Bit sets over Int32Array words for the in-segment NFA when it outgrows
// one int32 (the small case stays a plain int32 in the callers), and a
// class as the bytes it takes, which the pieces and the in-segment NFA
// test a byte against. JS twin of
// `crates/globstar/src/engine/segment/bits.rs`.

import { IS_WINDOWS_SEP } from "../../bytes.js";

export const wordsFor = (nBits) => Math.max(1, (nBits + 31) >>> 5);

export const bitTest = (a, i) => (a[i >>> 5] & (1 << (i & 31))) !== 0;

export function bitSet(a, i) {
  a[i >>> 5] |= 1 << (i & 31);
}

export function bitsIsZero(a) {
  for (let w = 0; w < a.length; w++) if (a[w] !== 0) return false;
  return true;
}

// A class as the bytes it takes, one bit each in eight int32 words, with
// its negation applied and the separators left out: a test is one
// lookup, however many items the class lists. `neg` stays for dot
// protection, where a negated class counts as a wildcard.
// Case-insensitively (`ci`) a listed ASCII letter lists its other case
// too, before the negation (§13).
export function byteSet(cls, ci) {
  const bits = [0, 0, 0, 0, 0, 0, 0, 0];
  // A word at a time: a class costs its ranges, not the bytes they span.
  for (const { lo, hi } of cls.ranges) {
    for (let w = lo >> 5; w <= hi >> 5; w++) {
      const from = w === lo >> 5 ? lo & 31 : 0;
      const to = w === hi >> 5 ? hi & 31 : 31;
      bits[w] |= (-1 >>> (31 - (to - from))) << from;
    }
  }
  const has = (b) => ((bits[b >> 5] >>> (b & 31)) & 1) === 1;
  for (let b = 0x41; b <= 0x5a; b++) {
    if (ci && (has(b) || has(b | 0x20))) {
      bits[b >> 5] |= 1 << (b & 31);
      bits[(b | 0x20) >> 5] |= 1 << ((b | 0x20) & 31);
    }
  }
  if (cls.neg) for (let w = 0; w < 8; w++) bits[w] = ~bits[w];
  bits[0x2f >> 5] &= ~(1 << (0x2f & 31));
  if (IS_WINDOWS_SEP) bits[0x5c >> 5] &= ~(1 << (0x5c & 31));
  return { neg: cls.neg, bits };
}

export const setHas = (set, c) => ((set.bits[c >> 5] >>> (c & 31)) & 1) === 1;
export const setEmpty = (set) => set.bits.every((w) => w === 0);
