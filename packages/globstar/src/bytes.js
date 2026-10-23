// Byte and bit primitives the engines share: separators, ASCII case
// folding and the comparisons built on them. Rust twin:
// `crates/globstar/src/bytes.rs` (plus what Rust gets from std,
// `u32::trailing_zeros` and `u8::eq_ignore_ascii_case`).

// Is `\` a separator on this platform (Windows only)? Path separators
// per GLOB_SPEC §2.2: `/` always, `\` on Windows.
export const IS_WINDOWS_SEP = typeof process !== "undefined" && process.platform === "win32";
export function isPathSep(b) {
  return b === 0x2f || (IS_WINDOWS_SEP && b === 0x5c);
}

// Trailing-zero count of a nonzero 32-bit word. Undefined for 0 — every
// caller iterates set bits under a `word !== 0` guard.
export const ctz32 = (v) => 31 - Math.clz32(v & -v);

// Set bits of a 32-bit word.
export function popcnt32(v) {
  v -= (v >>> 1) & 0x55555555;
  v = (v & 0x33333333) + ((v >>> 2) & 0x33333333);
  return Math.imul((v + (v >>> 4)) & 0x0f0f0f0f, 0x01010101) >>> 24;
}

// ASCII case-insensitive byte equality, the twin of Rust's
// `u8::eq_ignore_ascii_case`: lowercase both via the `0x20` case bit,
// then require a letter so non-letter pairs (`@`/`` ` ``) stay unequal.
function eqByteCi(a, b) {
  if (a === b) return true;
  const l = a | 0x20;
  return l === (b | 0x20) && l >= 0x61 && l <= 0x7a;
}

// Does the literal `str` hold an escaped `\`? On Windows that is a
// separator no path segment holds, so the literal matches nothing
// (§4.2); elsewhere it is a plain char.
export const hasEscapedSep = (str) => IS_WINDOWS_SEP && str.includes("\\");

// `c`, ASCII-lowercased when case-insensitive.
export const fold = (c, ci) => (ci && c >= 0x41 && c <= 0x5a ? c + 32 : c);

// A pattern byte against a path byte: `/` takes any separator, letters
// fold when `ci`, and a separator byte of the pattern (an escaped `\`
// on Windows) takes nothing (§2.2). The unequal case, the common one in
// the prefilter, returns before that last test reads the constant.
export function patByteEq(pat, b, ci) {
  if (pat === 0x2f) return isPathSep(b);
  if (ci ? !eqByteCi(pat, b) : pat !== b) return false;
  return !(IS_WINDOWS_SEP && b === 0x5c);
}

// Does `str` hold `part` at `at`, ASCII case folded when `ci`?
export function affixEq(part, str, at, ci) {
  if (!ci) return str.startsWith(part, at);
  for (let i = 0; i < part.length; i++) {
    if (!eqByteCi(part.charCodeAt(i), str.charCodeAt(at + i))) return false;
  }
  return true;
}

// Is `str[s, t)` the literal `lit`, ASCII case folded when `ci`?
export function litEq(lit, str, s, t, ci) {
  return t - s === lit.length && affixEq(lit, str, s, ci);
}

// Does `str` end with the pattern bytes `suffix` (`patByteEq`)? Last
// byte first: that is where extensions differ.
export function endsWithPattern(str, suffix, ci) {
  const at = str.length - suffix.length;
  if (at < 0) return false;
  for (let i = suffix.length - 1; i >= 0; i--) {
    if (!patByteEq(suffix.charCodeAt(i), str.charCodeAt(at + i), ci)) return false;
  }
  return true;
}

// A folded literal against `str[s, t)` in the order sorted literals are
// kept, folding the path's characters as it goes: ASCII letters
// lowercased when case-insensitive, a separator as `/`.
export function cmpFolded(lit, str, s, t, ci) {
  const n = Math.min(lit.length, t - s);
  for (let i = 0; i < n; i++) {
    let c = str.charCodeAt(s + i);
    if (ci && c >= 0x41 && c <= 0x5a) c += 32;
    else if (IS_WINDOWS_SEP && c === 0x5c) c = 0x2f;
    const d = lit.charCodeAt(i) - c;
    if (d !== 0) return d;
  }
  return lit.length - (t - s);
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
