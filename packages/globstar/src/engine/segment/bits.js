// Bit sets over Int32Array words for the NFA simulations that outgrow
// one int32. JS twin of `crates/globstar/src/engine/segment/bits.rs`;
// the small case stays a plain int32 in the callers.

import { ctz32 } from "../../bytes.js";

export const wordsFor = (nBits) => Math.max(1, (nBits + 31) >>> 5);

export const bitTest = (a, i) => (a[i >>> 5] & (1 << (i & 31))) !== 0;

export function bitSet(a, i) {
  a[i >>> 5] |= 1 << (i & 31);
}

export function bitsOr(dst, src) {
  for (let w = 0; w < dst.length; w++) dst[w] |= src[w];
}

export function bitsIsZero(a) {
  for (let w = 0; w < a.length; w++) if (a[w] !== 0) return false;
  return true;
}

export function bitsEqual(a, b) {
  for (let w = 0; w < a.length; w++) if (a[w] !== b[w]) return false;
  return true;
}

// Visit every set bit, ascending.
export function bitsForEach(a, f) {
  for (let w = 0; w < a.length; w++) {
    let bits = a[w];
    while (bits !== 0) {
      f(w * 32 + ctz32(bits));
      bits &= bits - 1;
    }
  }
}
