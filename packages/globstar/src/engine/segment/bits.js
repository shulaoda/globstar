// Bit sets over Int32Array words for the in-segment NFA when it outgrows
// one int32. JS twin of `crates/globstar/src/engine/segment/bits.rs`;
// the small case stays a plain int32 in the callers.

export const wordsFor = (nBits) => Math.max(1, (nBits + 31) >>> 5);

export const bitTest = (a, i) => (a[i >>> 5] & (1 << (i & 31))) !== 0;

export function bitSet(a, i) {
  a[i >>> 5] |= 1 << (i & 31);
}

export function bitsIsZero(a) {
  for (let w = 0; w < a.length; w++) if (a[w] !== 0) return false;
  return true;
}
