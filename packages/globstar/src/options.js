// Bit `0x20` is ASCII's case bit: setting it lowercases, clearing it
// uppercases; any non-letter byte is returned unchanged.
export function asciiCaseAlt(b) {
  if (b >= 0x41 && b <= 0x5a) return b | 0x20;
  if (b >= 0x61 && b <= 0x7a) return b & ~0x20;
  return b;
}

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

// ASCII case-insensitive byte equality; non-ASCII bytes compare verbatim.
// Fold one side only — toggling both via asciiCaseAlt just swaps them
// (`r`/`R` → `R`/`r`) and still compares unequal.
export function eqByteCi(a, b) {
  if (a === b) return true;
  return asciiCaseAlt(a) === b;
}
