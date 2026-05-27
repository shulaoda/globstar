// Tier 0 — pure literal matcher. Routed to whenever the parsed pattern
// has no metacharacters at all. A single byte-by-byte compare with
// platform-separator normalization (GLOB_SPEC §12.3): each `/` in the
// pattern consumes exactly one separator byte from the path.

import { isPathSep, eqByteCi } from "../options.js";
import { DirMatch } from "../dir-match.js";

export class LiteralMatcher {
  constructor(literal, caseInsensitive) {
    this.literal = literal; // Uint8Array
    this.caseInsensitive = caseInsensitive;
  }

  isMatch(path) {
    return pathEq(this.literal, path, this.caseInsensitive);
  }

  matchDir(dirPath) {
    if (pathEq(this.literal, dirPath, this.caseInsensitive)) return DirMatch.Match;
    if (literalUnder(this.literal, dirPath, this.caseInsensitive)) return DirMatch.Descend;
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
