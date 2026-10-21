const ENCODER = new TextEncoder();

// How many UTF-8 bytes `str[s, t)` takes, without making them (what `?`
// counts). A lone surrogate encodes as U+FFFD, three bytes.
export function utf8Length(str, s = 0, t = str.length) {
  let n = t - s;
  for (let i = s; i < t; i++) {
    const c = str.charCodeAt(i);
    if (c < 0x80) continue;
    if (c < 0x800) n += 1;
    else if ((c & 0xfc00) === 0xd800 && i + 1 < t && (str.charCodeAt(i + 1) & 0xfc00) === 0xdc00) {
      n += 2;
      i++;
    } else n += 2;
  }
  return n;
}

// Bytes (a Uint8Array or a plain array of bytes) → a Latin-1 string,
// each byte one char code: the byte-space string `utf8Latin1` builds
// when there is no `Buffer`, and the copy `flat` makes. `[]` yields `""`.
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
        : latin1(ENCODER.encode(input));
    }
  }
  return input;
}

// `s` as a flat string of its own. V8 keeps a string of 13 or more chars
// joined from parts as a tree of them, and a slice as a view that keeps
// its source alive; the strings a matcher stores live as long as it does.
export function flat(s) {
  if (s.length < 13) return s;
  const codes = [];
  for (let i = 0; i < s.length; i++) codes.push(s.charCodeAt(i));
  return latin1(codes);
}

// Is every char of `str[s, t)` ASCII?
export function isAscii(str, s, t) {
  for (let i = s; i < t; i++) if (str.charCodeAt(i) > 0x7f) return false;
  return true;
}

// Inverse of `latin1`: a Latin-1 string back to its bytes.
export function latin1Bytes(str) {
  const out = new Uint8Array(str.length);
  for (let i = 0; i < str.length; i++) out[i] = str.charCodeAt(i);
  return out;
}
