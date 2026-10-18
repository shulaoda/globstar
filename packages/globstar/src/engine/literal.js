import { isPathSep, eqByteCi, IS_WINDOWS_SEP } from "../bytes.js";
import { DirMatch } from "../dir-match.js";
import { flat, latin1Bytes, utf8Latin1 } from "../utf8.js";

export class LiteralMatcher {
  // `litStr` holds the literal's bytes, one char each (see `utf8Latin1`).
  // An ASCII literal is compared against the raw path string: a
  // non-ASCII char equals no ASCII byte either way.
  constructor(litStr, caseInsensitive, ascii) {
    this.caseInsensitive = caseInsensitive;
    this.litStr = flat(litStr);
    this.ascii = ascii;
    this.exact = this.ascii && !caseInsensitive && !IS_WINDOWS_SEP;
  }

  // See `SegmentMatcher.shape` (a test hook).
  shape() {
    return "literal";
  }

  isMatch(path) {
    if (this.exact) return path === this.litStr;
    const str = this.ascii ? path : utf8Latin1(path);
    return (
      str.length === this.litStr.length &&
      prefixLen(this.litStr, str, this.caseInsensitive) === str.length
    );
  }

  matchDir(dirPath) {
    const dir = this.ascii ? dirPath : utf8Latin1(dirPath);
    const lit = this.litStr;
    const n = prefixLen(lit, dir, this.caseInsensitive);
    if (n === dir.length && n === lit.length) return DirMatch.Match;
    if (dir.length === 0 || (n === dir.length && lit.charCodeAt(n) === 0x2f)) {
      return DirMatch.Descend;
    }
    return DirMatch.Pruned;
  }

  staticPrefixes() {
    const lit = this.litStr;
    let end = lit.length;
    while (end > 0 && lit.charCodeAt(end - 1) === 0x2f) end--;
    return [latin1Bytes(lit.slice(0, end))];
  }
}

// How many leading chars of `str` the literal takes. A pattern `/` takes
// any separator byte; no other byte ever does (§2.2).
function prefixLen(lit, str, ci) {
  const end = lit.length < str.length ? lit.length : str.length;
  for (let n = 0; n < end; n++) {
    const lb = lit.charCodeAt(n);
    const pb = str.charCodeAt(n);
    if (lb === 0x2f ? isPathSep(pb) : !isPathSep(pb) && (ci ? eqByteCi(lb, pb) : lb === pb)) {
      continue;
    }
    return n;
  }
  return end;
}
