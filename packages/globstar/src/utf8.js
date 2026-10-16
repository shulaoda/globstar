const ENCODER = new TextEncoder();

// A string's UTF-8 bytes.
export function toBytes(input) {
  const n = input.length;
  for (let i = 0; i < n; i++) {
    if (input.charCodeAt(i) > 0x7f) return ENCODER.encode(input);
  }
  // ASCII fast path: each char is its own byte, skip TextEncoder.
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = input.charCodeAt(i);
  return out;
}

// How many bytes `toBytes(input)` would return, without making them. A
// lone surrogate encodes as U+FFFD, three bytes.
export function utf8Length(input) {
  let n = 0;
  for (let i = 0; i < input.length; i++) {
    const c = input.charCodeAt(i);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c < 0xdc00 && (input.charCodeAt(i + 1) & 0xfc00) === 0xdc00) {
      n += 4;
      i++;
    } else n += 3;
  }
  return n;
}

// Bytes (a Uint8Array or a plain array of bytes) → a Latin-1 string,
// each byte one char code. The inverse of the ASCII/byte path above,
// used to hold short pattern literals as strings for the string-mode
// matcher. `[]` yields `""`.
const CHUNK = 4096; // `fromCharCode.apply` has an argument-count ceiling.
export function latin1(bytes) {
  const n = bytes.length;
  if (n <= CHUNK) return n === 0 ? "" : String.fromCharCode.apply(null, bytes);
  let out = "";
  for (let i = 0; i < n; i += CHUNK) {
    out += String.fromCharCode.apply(null, bytes.slice(i, i + CHUNK));
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
