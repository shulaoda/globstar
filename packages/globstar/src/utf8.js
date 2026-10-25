const ENCODER = new TextEncoder();

const CHUNK = 4096;

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

function latin1(bytes) {
  const n = bytes.length;
  if (n <= CHUNK) return n === 0 ? "" : String.fromCharCode.apply(null, bytes);
  let out = "";
  for (let i = 0; i < n; i += CHUNK) {
    out += String.fromCharCode.apply(null, bytes.slice(i, i + CHUNK));
  }
  return out;
}

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

export function flat(s) {
  if (s.length < 13) return s;
  const codes = [];
  for (let i = 0; i < s.length; i++) codes.push(s.charCodeAt(i));
  return latin1(codes);
}

export function isAscii(str, s, t) {
  for (let i = s; i < t; i++) if (str.charCodeAt(i) > 0x7f) return false;
  return true;
}

export function latin1Bytes(str) {
  const out = new Uint8Array(str.length);
  for (let i = 0; i < str.length; i++) out[i] = str.charCodeAt(i);
  return out;
}
