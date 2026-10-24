// Byte primitives the engines share: separators, ASCII case folding and
// the comparisons built on them.

export const IS_WINDOWS_SEP = typeof process !== "undefined" && process.platform === "win32";

export function isPathSep(b) {
  return b === 0x2f || (IS_WINDOWS_SEP && b === 0x5c);
}

// Undefined for 0: every caller iterates set bits under a `word !== 0` guard.
export const ctz32 = (v) => 31 - Math.clz32(v & -v);

// Lowercase both through the case bit, then require a letter, so that
// non-letter pairs (`@`, `` ` ``) stay unequal.
function eqByteCi(a, b) {
  if (a === b) return true;
  const l = a | 0x20;
  return l === (b | 0x20) && l >= 0x61 && l <= 0x7a;
}

// On Windows an escaped `\` is a separator no segment holds (GLOB_SPEC §4.2).
export const hasEscapedSep = (str) => IS_WINDOWS_SEP && str.includes("\\");

export const fold = (c, ci) => (ci && c >= 0x41 && c <= 0x5a ? c + 32 : c);

// A pattern byte against a path byte: `/` takes any separator, and a
// separator byte of the pattern takes nothing (GLOB_SPEC §2.2).
export function patByteEq(pat, b, ci) {
  if (pat === 0x2f) return isPathSep(b);
  if (ci ? !eqByteCi(pat, b) : pat !== b) return false;
  return !(IS_WINDOWS_SEP && b === 0x5c);
}

export function affixEq(part, str, at, ci) {
  if (!ci) return str.startsWith(part, at);
  for (let i = 0; i < part.length; i++) {
    if (!eqByteCi(part.charCodeAt(i), str.charCodeAt(at + i))) return false;
  }
  return true;
}

export function litEq(lit, str, s, t, ci) {
  return t - s === lit.length && affixEq(lit, str, s, ci);
}

// Last byte first: that is where extensions differ.
export function endsWithPattern(str, suffix, ci) {
  const at = str.length - suffix.length;
  if (at < 0) return false;
  for (let i = suffix.length - 1; i >= 0; i--) {
    if (!patByteEq(suffix.charCodeAt(i), str.charCodeAt(at + i), ci)) return false;
  }
  return true;
}

// A folded literal against `str[s, t)` in the order sorted literals are
// kept: the path's letters lowercased when `ci`, its separators as `/`.
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

export function nextSep(str, from) {
  const i = str.indexOf("/", from);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.indexOf("\\", from);
  if (i === -1) return j;
  if (j === -1) return i;
  return i < j ? i : j;
}

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
