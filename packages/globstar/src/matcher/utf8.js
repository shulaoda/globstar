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
