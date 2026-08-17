import { isPathSep, eqByteCi, IS_WINDOWS_SEP } from "../bytes.js";
import { DirMatch } from "../dir-match.js";
import { toBytes, latin1 } from "../utf8.js";

export class LiteralMatcher {
  constructor(literal, caseInsensitive) {
    this.literal = literal;
    this.caseInsensitive = caseInsensitive;
    let ascii = true;
    for (let i = 0; i < literal.length; i++) {
      if (literal[i] > 0x7f) {
        ascii = false;
        break;
      }
    }
    this.litStr = ascii ? latin1(literal) : null;
    this.exact = ascii && !caseInsensitive && !IS_WINDOWS_SEP;
  }

  isMatch(path) {
    if (this.exact) return path === this.litStr;
    if (this.litStr === null) return this.pathEq(toBytes(path));
    return this.pathEqStr(path);
  }

  matchDir(dirPath) {
    const dir = toBytes(dirPath);
    if (this.pathEq(dir)) return DirMatch.Match;
    if (this.literalUnder(dir)) return DirMatch.Descend;
    return DirMatch.Pruned;
  }

  staticPrefixes() {
    const bytes = this.literal;
    let end = bytes.length;
    while (end > 0 && bytes[end - 1] === 0x2f) end--;
    return [bytes.slice(0, end)];
  }

  matchPrefix(other) {
    const literal = this.literal;
    const ci = this.caseInsensitive;
    let n = 0;
    const llen = literal.length,
      olen = other.length;
    while (n < llen && n < olen) {
      const lb = literal[n];
      const ob = other[n];
      if (lb === 0x2f ? isPathSep(ob) : !isPathSep(ob) && (ci ? eqByteCi(lb, ob) : lb === ob)) {
        n++;
      } else {
        break;
      }
    }
    return n;
  }

  pathEq(path) {
    const n = this.matchPrefix(path);
    return n === this.literal.length && n === path.length;
  }

  literalUnder(dirPath) {
    if (dirPath.length === 0) return true;
    const n = this.matchPrefix(dirPath);
    return n === dirPath.length && n < this.literal.length && this.literal[n] === 0x2f;
  }

  pathEqStr(path) {
    const lit = this.litStr;
    const ci = this.caseInsensitive;
    const llen = lit.length;
    if (path.length !== llen) return false;
    for (let i = 0; i < llen; i++) {
      const lb = lit.charCodeAt(i);
      const pb = path.charCodeAt(i);
      if (lb === 0x2f ? isPathSep(pb) : !isPathSep(pb) && (ci ? eqByteCi(lb, pb) : lb === pb)) {
        continue;
      }
      return false;
    }
    return true;
  }
}
