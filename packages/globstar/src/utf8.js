// Shared UTF-8 encoding helper.
//
// Patterns and input paths must reach the engines as the same byte
// space — pattern bytes are produced here at parse time, input bytes at
// the public matcher boundary. ASCII fast path avoids `TextEncoder`'s
// fixed setup cost; non-ASCII falls through to UTF-8 encoding (rare in
// glob patterns, common in real filesystem paths).

const ENCODER = typeof TextEncoder !== "undefined" ? new TextEncoder() : null;

export function toBytes(input) {
  if (input instanceof Uint8Array) return input;
  const n = input.length;
  for (let i = 0; i < n; i++) {
    if (input.charCodeAt(i) > 0x7f) {
      return ENCODER !== null ? ENCODER.encode(input) : Uint8Array.from(Buffer.from(input, "utf8"));
    }
  }
  // ASCII fast path: each char is its own byte, skip TextEncoder.
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = input.charCodeAt(i);
  return out;
}

// Bytes → a Latin-1 string (each byte becomes one char code). The inverse
// of the ASCII/byte path above, used to hold short pattern literals as
// strings for the string-mode matcher. `[]` yields `""`.
const CHUNK = 4096; // `fromCharCode.apply` has an argument-count ceiling.
export function latin1(bytes) {
  const n = bytes.length;
  if (n <= CHUNK) return n === 0 ? "" : String.fromCharCode.apply(null, bytes);
  let out = "";
  for (let i = 0; i < n; i += CHUNK) {
    out += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
  }
  return out;
}

// A path string in the byte space: one char per UTF-8 byte. ASCII is
// already its own UTF-8, so it passes through with no allocation;
// `Buffer` does the transcode in one native step where it exists.
export function utf8Latin1(input) {
  const n = input.length;
  for (let i = 0; i < n; i++) {
    if (input.charCodeAt(i) > 0x7f) {
      return typeof Buffer !== "undefined"
        ? Buffer.from(input, "utf8").toString("latin1")
        : latin1(toBytes(input));
    }
  }
  return input;
}

// Inverse of `latin1`: a Latin-1 string back to its bytes.
export function latin1Bytes(str) {
  const out = new Uint8Array(str.length);
  for (let i = 0; i < str.length; i++) out[i] = str.charCodeAt(i);
  return out;
}
