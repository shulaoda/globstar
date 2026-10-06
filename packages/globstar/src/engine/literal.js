import { isPathSep, eqByteCi, IS_WINDOWS_SEP } from "../bytes.js";
import { DirMatch } from "../dir-match.js";
import { latin1, utf8Latin1 } from "../utf8.js";

export class LiteralMatcher {
  constructor(literal, caseInsensitive) {
    this.literal = literal;
    this.caseInsensitive = caseInsensitive;
    // One char per literal byte (see `utf8Latin1`). An ASCII literal is
    // compared against the raw path string: a non-ASCII char equals no
    // ASCII byte either way.
    this.litStr = latin1(literal);
    this.ascii = literal.every((b) => b <= 0x7f);
    this.exact = this.ascii && !caseInsensitive && !IS_WINDOWS_SEP;
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
    const bytes = this.literal;
    let end = bytes.length;
    while (end > 0 && bytes[end - 1] === 0x2f) end--;
    return [bytes.slice(0, end)];
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
