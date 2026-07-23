// Tier 0 — pure literal matcher. Routed to whenever the parsed pattern
// has no metacharacters at all. A single byte-by-byte compare with
// platform-separator normalization (GLOB_SPEC §12.3): each `/` in the
// pattern consumes exactly one separator byte from the path.

import { isPathSep, eqByteCi } from "../options.js";
import { DirMatch } from "../dir-match.js";
import { toBytes } from "../utf8.js";

const IS_WINDOWS_SEP = isPathSep(0x5c);

export class LiteralMatcher {
  constructor(literal, caseInsensitive) {
    this.literal = literal; // Uint8Array
    this.caseInsensitive = caseInsensitive;
    // String fast path: valid only for all-ASCII literals (char code
    // ⇔ byte is 1:1 and any non-ASCII input char mismatches an ASCII
    // byte in both modes). On POSIX without case folding the
    // separator-normalized compare degenerates to plain string
    // equality — a single intrinsic.
    this.acceptsStrings = true;
    let ascii = true;
    for (let i = 0; i < literal.length; i++) {
      if (literal[i] > 0x7f) {
        ascii = false;
        break;
      }
    }
    this.litStr = ascii ? String.fromCharCode.apply(null, literal) : null;
    this.exactStr = ascii && !caseInsensitive && !IS_WINDOWS_SEP ? this.litStr : null;
  }

  isMatch(path) {
    if (typeof path === "string") {
      if (this.exactStr !== null) return path === this.exactStr;
      if (this.litStr === null) return pathEq(this.literal, toBytes(path), this.caseInsensitive);
      return pathEqStr(this.litStr, path, this.caseInsensitive);
    }
    return pathEq(this.literal, path, this.caseInsensitive);
  }

  matchDir(dirPath) {
    const dir = typeof dirPath === "string" ? toBytes(dirPath) : dirPath;
    if (pathEq(this.literal, dir, this.caseInsensitive)) return DirMatch.Match;
    if (literalUnder(this.literal, dir, this.caseInsensitive)) return DirMatch.Descend;
    return DirMatch.Pruned;
  }

  // Tier-0 prefix: the literal itself, with any trailing separators
  // stripped (walker re-adds them when descending).
  staticPrefixes() {
    const bytes = this.literal;
    let end = bytes.length;
    while (end > 0 && bytes[end - 1] === 0x2f) end--;
    return [bytes.slice(0, end)];
  }
}

// String-input twin of `pathEq` for all-ASCII literals (Windows or
// case-insensitive — POSIX case-sensitive uses `exactStr` equality).
function pathEqStr(lit, path, ci) {
  const llen = lit.length;
  if (path.length !== llen) return false;
  for (let i = 0; i < llen; i++) {
    const lb = lit.charCodeAt(i);
    const pb = path.charCodeAt(i);
    if (lb === 0x2f ? isPathSep(pb) : ci ? eqByteCi(lb, pb) : lb === pb) continue;
    return false;
  }
  return true;
}

// Whole-path equality with separator normalization. Both args are
// `Uint8Array` — the public boundary in `compileMatcher`'s `match` /
// `matchDir` closures encodes any JS-string input via `pathBytes()`.
function pathEq(literal, path, ci) {
  let li = 0,
    pi = 0;
  const llen = literal.length,
    plen = path.length;
  while (li < llen && pi < plen) {
    const lb = literal[li];
    const pb = path[pi];
    if (lb === 0x2f ? isPathSep(pb) : ci ? eqByteCi(lb, pb) : lb === pb) {
      li++;
      pi++;
    } else {
      return false;
    }
  }
  return li === llen && pi === plen;
}

// Whether `literal` lives strictly under `dirPath` — used by `matchDir`
// to answer "should the walker descend into this directory?". Empty
// `dirPath` (cwd) is "under" everything.
function literalUnder(literal, dirPath, ci) {
  if (dirPath.length === 0) return true;
  let li = 0,
    di = 0;
  const llen = literal.length,
    dlen = dirPath.length;
  while (li < llen && di < dlen) {
    const lb = literal[li];
    const db = dirPath[di];
    if (lb === 0x2f ? isPathSep(db) : ci ? eqByteCi(lb, db) : lb === db) {
      li++;
      di++;
    } else {
      return false;
    }
  }
  return di === dlen && li < llen && literal[li] === 0x2f;
}
