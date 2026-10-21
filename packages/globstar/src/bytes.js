// Byte and bit primitives: what the Rust side gets from std
// (`std::path::is_separator`, `u32::trailing_zeros`,
// `u8::eq_ignore_ascii_case`) and the twins of `pat_byte_eq`
// (`engine/mod.rs`) and `seg_end` / `seg_start` (`engine/segment/mod.rs`).

// Is `\` a separator on this platform (Windows only)? Path separators
// per GLOB_SPEC §2.2: `/` always, `\` on Windows.
export const IS_WINDOWS_SEP = typeof process !== "undefined" && process.platform === "win32";
export function isPathSep(b) {
  return b === 0x2f || (IS_WINDOWS_SEP && b === 0x5c);
}

// Trailing-zero count of a nonzero 32-bit word. Undefined for 0 — every
// caller iterates set bits under a `word !== 0` guard.
export const ctz32 = (v) => 31 - Math.clz32(v & -v);

// ASCII case-insensitive byte equality, the twin of Rust's
// `u8::eq_ignore_ascii_case`: lowercase both via the `0x20` case bit,
// then require a letter so non-letter pairs (`@`/`` ` ``) stay unequal.
export function eqByteCi(a, b) {
  if (a === b) return true;
  const l = a | 0x20;
  return l === (b | 0x20) && l >= 0x61 && l <= 0x7a;
}

// A pattern byte against a path byte: `/` takes any separator, letters
// fold when `ci`, and a separator byte of the pattern (an escaped `\`
// on Windows) takes nothing (§2.2). The unequal case, the common one in
// the prefilter, returns before that last test reads the constant.
export function patByteEq(pat, b, ci) {
  if (pat === 0x2f) return isPathSep(b);
  if (ci ? !eqByteCi(pat, b) : pat !== b) return false;
  return !(IS_WINDOWS_SEP && b === 0x5c);
}

// Index of the first separator in `str` from `from` on, or -1.
export function nextSep(str, from) {
  const i = str.indexOf("/", from);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.indexOf("\\", from);
  if (i === -1) return j;
  if (j === -1) return i;
  return i < j ? i : j;
}

// Index of the last separator in `str` before `end`, or -1.
export function lastSepBefore(str, end) {
  if (end <= 0) return -1;
  const i = str.lastIndexOf("/", end - 1);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.lastIndexOf("\\", end - 1);
  return i > j ? i : j;
}

export function asciiLower(s) {
  let out = "";
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    out += c >= 0x41 && c <= 0x5a ? String.fromCharCode(c + 32) : s[i];
  }
  return out;
}
