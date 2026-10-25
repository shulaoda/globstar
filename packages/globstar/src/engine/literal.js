import { IS_WINDOWS_SEP, endsWithPattern, hasEscapedSep, patByteEq } from "./bytes.js";
import { DirMatch } from "../dir-match.js";
import { latin1Bytes, utf8Latin1 } from "../utf8.js";

export class LiteralMatcher {
  constructor(litStr, caseInsensitive, ascii) {
    this.caseInsensitive = caseInsensitive;
    this.litStr = litStr;
    this.ascii = ascii;
    this.exact = this.ascii && !caseInsensitive && !IS_WINDOWS_SEP;
  }

  shape() {
    return "literal";
  }

  isMatch(path) {
    if (this.exact) return path === this.litStr;
    const str = this.ascii ? path : utf8Latin1(path);
    return (
      str.length === this.litStr.length && endsWithPattern(str, this.litStr, this.caseInsensitive)
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
    if (hasEscapedSep(lit)) return [];
    let end = lit.length;
    while (end > 0 && lit.charCodeAt(end - 1) === 0x2f) end--;
    return [latin1Bytes(lit.slice(0, end))];
  }
}

function prefixLen(lit, str, ci) {
  const end = lit.length < str.length ? lit.length : str.length;
  let n = 0;
  while (n < end && patByteEq(lit.charCodeAt(n), str.charCodeAt(n), ci)) n++;
  return n;
}
