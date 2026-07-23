// Bit 5 (0x20) is the ASCII case-toggle bit: A(0x41) ↔ a(0x61).
export function asciiCaseAlt(b) {
  if (b >= 0x41 && b <= 0x5a) return b | 0x20; // A-Z → a-z
  if (b >= 0x61 && b <= 0x7a) return b & ~0x20; // a-z → A-Z
  return b;
}

// Path separator set per GLOB_SPEC §12.3: `/` always, `\` on Windows.
const IS_WINDOWS = typeof process !== "undefined" && process.platform === "win32";
export function isPathSep(b) {
  if (b === 0x2f) return true;
  if (IS_WINDOWS && b === 0x5c) return true;
  return false;
}

// ASCII case-insensitive byte equality. Non-ASCII bytes compare
// verbatim. Toggling both sides via `asciiCaseAlt` was a bug: it just
// swaps them, so two letters that differ only in case (e.g. `r` 0x72
// and `R` 0x52) end up as `(0x52, 0x72)` and still compare unequal.
// Toggling one side gives the correct fold.
export function eqByteCi(a, b) {
  if (a === b) return true;
  return asciiCaseAlt(a) === b;
}
