// Byte and bit primitives the Rust side gets from std:
// `std::path::is_separator`, `u32::trailing_zeros`,
// `u8::eq_ignore_ascii_case`.

// Path separators per GLOB_SPEC §12.3: `/` always, `\` on Windows.
const IS_WINDOWS = typeof process !== "undefined" && process.platform === "win32";
export function isPathSep(b) {
  if (b === 0x2f) return true;
  if (IS_WINDOWS && b === 0x5c) return true;
  return false;
}

// Is `\` a separator on this platform (Windows only)?
export const IS_WINDOWS_SEP = isPathSep(0x5c);

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
