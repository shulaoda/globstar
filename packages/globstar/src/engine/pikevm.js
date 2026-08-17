import {
  T_BYTE,
  T_CLASS,
  T_ANY_NON_SEP,
  T_ANY_BYTE,
  T_SEP,
  T_SPLIT,
  T_JUMP,
  T_DOT_GUARD,
  T_MATCH,
  T_NULL,
  compileThompson,
} from "./thompson.js";
import { isPathSep, ctz32 } from "../bytes.js";
import { classMatches } from "../ast.js";
import { DirMatch } from "../dir-match.js";
import { computeStaticPrefixes } from "./ops/index.js";
import { toBytes, latin1Bytes } from "../utf8.js";
import { GlobError } from "../error.js";

function reachFromClosures(closures, infoOff, acceptOff, nWords) {
  const n = closures.length - infoOff;
  const reach = new Uint8Array(n);
  let changed = true;
  while (changed) {
    changed = false;
    for (let s = n - 1; s >= 0; s--) {
      if (reach[s]) continue;
      const word2 = closures[infoOff + s];
      const tag = word2 & 0xf;
      if (tag === T_NULL || tag === T_MATCH) continue;
      const base = (word2 >>> 16) * nWords;
      let hit = false;
      for (let w = 0; w < nWords && !hit; w++) {
        const cls = closures[base + w];
        if (cls === 0) continue;
        if (cls & closures[acceptOff + w]) {
          hit = true;
          break;
        }
        let word = cls;
        while (word !== 0) {
          const off = ctz32(word);
          const s2 = (w << 5) + off;
          word &= word - 1;
          if (reach[s2]) {
            hit = true;
            break;
          }
        }
      }
      if (hit) {
        reach[s] = 1;
        changed = true;
      }
    }
  }
  return reach;
}

function staticClosuresN(tags, nexts, splitsB, n, nWords, out) {
  const seen = new Uint8Array(n);
  const stack = [];
  for (let root = 0; root < n; root++) {
    if (seen[root]) continue;
    stack.push(root);
    while (stack.length > 0) {
      const item = stack.pop();
      if (item < 0) {
        const s = ~item;
        const tag = tags[s];
        const base = s * nWords;
        if (tag === T_SPLIT) {
          const aBase = nexts[s] * nWords;
          const bBase = splitsB[s] * nWords;
          for (let w = 0; w < nWords; w++) out[base + w] = out[aBase + w] | out[bBase + w];
        } else if (tag === T_JUMP) {
          const nxBase = nexts[s] * nWords;
          for (let w = 0; w < nWords; w++) out[base + w] = out[nxBase + w];
        } else {
          out[base + (s >>> 5)] = 1 << (s & 31);
        }
        continue;
      }

      const s = item;
      if (seen[s]) continue;
      seen[s] = 1;
      stack.push(~s);
      const tag = tags[s];
      if (tag === T_SPLIT) {
        if (!seen[nexts[s]]) stack.push(nexts[s]);
        if (!seen[splitsB[s]]) stack.push(splitsB[s]);
      } else if (tag === T_JUMP) {
        if (!seen[nexts[s]]) stack.push(nexts[s]);
      }
    }
  }
}

export class PikeVm {
  constructor(nfa, facts, prefixes) {
    this.facts = facts;
    this.prefixes = prefixes;

    const n = nfa.n;
    if (n > 0x10000) throw new GlobError("TooManyStates", { n, max: 0x10000 });
    const nWords = Math.max(1, (n + 31) >>> 5);
    this.nWords = nWords;

    const acceptOff = n * nWords;
    const infoOff = acceptOff + nWords;
    const combined = new Uint32Array(infoOff + n);
    staticClosuresN(nfa.tags, nfa.nexts, nfa.splitsB, n, nWords, combined);
    const tags = nfa.tags;
    const nexts = nfa.nexts;
    const byteVals = nfa.byteVals;
    const inClsRefs = nfa.clsRefs;
    const protectFlag = nfa.dot ? 0 : 0x10;
    let clsRefs = null;
    const guardIds = [];
    for (let i = 0; i < n; i++) {
      const tag = tags[i];
      if (tag === T_SPLIT || tag === T_JUMP) {
        combined[infoOff + i] = T_NULL;
        continue;
      }
      let bits = tag | (byteVals[i] << 8) | (nexts[i] << 16);
      if (tag === T_ANY_NON_SEP || tag === T_ANY_BYTE || (tag === T_CLASS && inClsRefs[i].neg)) {
        bits |= protectFlag;
      }
      combined[infoOff + i] = bits;
      if (tag === T_DOT_GUARD) guardIds.push(i);
      if (tag === T_CLASS) {
        if (clsRefs === null) clsRefs = Array.from({ length: n });
        clsRefs[i] = inClsRefs[i];
      }
    }
    this.closures = combined;
    this.initOff = nfa.initial * nWords;
    this.acceptOff = acceptOff;
    this.infoOff = infoOff;
    this.clsRefs = clsRefs;

    let guardExps = null;
    if (guardIds.length > 0) {
      const stride = 1 + nWords;
      guardExps = new Uint32Array(guardIds.length * stride);
      for (let i = 0; i < guardIds.length; i++) {
        const g = guardIds[i];
        guardExps[i * stride] = g;
        const base = nexts[g] * nWords;
        for (let w = 0; w < nWords; w++) guardExps[i * stride + 1 + w] = combined[base + w];
      }

      let changed = true;
      while (changed) {
        changed = false;
        for (let i = guardIds.length - 1; i >= 0; i--) {
          for (let j = 0; j < guardIds.length; j++) {
            const gj = guardIds[j];
            if (i === j || (guardExps[i * stride + 1 + (gj >>> 5)] & (1 << (gj & 31))) === 0) {
              continue;
            }
            for (let w = 0; w < nWords; w++) {
              const merged = (guardExps[i * stride + 1 + w] | guardExps[j * stride + 1 + w]) >>> 0;
              if (merged !== guardExps[i * stride + 1 + w]) {
                guardExps[i * stride + 1 + w] = merged;
                changed = true;
              }
            }
          }
        }
      }
    }
    this.guardExps = guardExps;

    const accept = nfa.accept;
    combined[acceptOff + (accept >>> 5)] |= 1 << (accept & 31);
    if (guardExps !== null) {
      const stride = 1 + nWords;
      for (let r = 0; r < guardExps.length; r += stride) {
        if (guardExps[r + 1 + (accept >>> 5)] & (1 << (accept & 31))) {
          const g = guardExps[r];
          combined[acceptOff + (g >>> 5)] |= 1 << (g & 31);
        }
      }
    }

    this._scratch = new Uint32Array(nWords * 2);
    this._reachToAccept = null;
  }

  static build(program, dot) {
    const nfa = compileThompson(program, dot);
    return new PikeVm(nfa, program.facts, computeStaticPrefixes(program.ops));
  }

  staticPrefixes() {
    // Prefixes live as Latin-1 strings; the walker contract is bytes.
    return this.prefixes.map(latin1Bytes);
  }

  _isAccept(bits) {
    const closures = this.closures;
    const acceptOff = this.acceptOff;
    const nWords = this.nWords;
    for (let w = 0; w < nWords; w++) {
      if (bits[w] & closures[acceptOff + w]) return true;
    }
    return false;
  }

  isMatch(input) {
    const path = toBytes(input);
    if (!this.facts.accept(path)) return false;
    this._run(path);
    return this._isAccept(this._scratch);
  }

  matchDir(input) {
    const dirPath = toBytes(input);
    if (dirPath.length === 0) return DirMatch.fromExactPrefix(this.isMatch(""), true);
    this._run(dirPath);
    const exact = this._isAccept(this._scratch);
    return DirMatch.fromExactPrefix(exact, this._hasPrefixDescent());
  }

  _hasPrefixDescent() {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const nWords = this.nWords;
    const scratch = this._scratch;
    const nxtBase = nWords;

    const guardExps = this.guardExps;
    if (guardExps !== null) {
      const stride = 1 + nWords;
      for (let r = 0; r < guardExps.length; r += stride) {
        const g = guardExps[r];
        if (scratch[g >>> 5] & (1 << (g & 31))) {
          for (let j = 0; j < nWords; j++) scratch[j] |= guardExps[r + 1 + j];
        }
      }
    }

    for (let w = 0; w < nWords; w++) scratch[nxtBase + w] = 0;
    for (let w = 0; w < nWords; w++) {
      let word = scratch[w];
      while (word !== 0) {
        const off = ctz32(word);
        const s = (w << 5) + off;
        word &= word - 1;
        const word2 = closures[infoOff + s];
        const tag = word2 & 0xf;
        if (tag === T_SEP || tag === T_ANY_BYTE) {
          const base = (word2 >>> 16) * nWords;
          for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
        }
      }
    }
    const acceptOff = this.acceptOff;
    for (let w = 0; w < nWords; w++) {
      if (scratch[nxtBase + w] & closures[acceptOff + w]) return true;
    }
    let reach = this._reachToAccept;
    if (reach === null) {
      reach = reachFromClosures(closures, infoOff, acceptOff, nWords);
      this._reachToAccept = reach;
    }
    for (let w = 0; w < nWords; w++) {
      let word = scratch[nxtBase + w];
      while (word !== 0) {
        const off = ctz32(word);
        const s = (w << 5) + off;
        word &= word - 1;
        if (reach[s]) return true;
      }
    }
    return false;
  }

  _run(path) {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const clsRefs = this.clsRefs;
    const nWords = this.nWords;
    const scratch = this._scratch;
    const guardExps = this.guardExps;
    const stride = 1 + nWords;
    const nxtBase = nWords;
    const initOff = this.initOff;
    for (let w = 0; w < nWords; w++) scratch[w] = closures[initOff + w];

    let atSegStart = true;

    for (let p = 0; p < path.length; p++) {
      const c = path[p];
      const sep = isPathSep(c);
      const dotMaskFlag = atSegStart && c === 0x2e ? 0x10 : 0;

      if (guardExps !== null && dotMaskFlag === 0) {
        for (let r = 0; r < guardExps.length; r += stride) {
          const g = guardExps[r];
          if (scratch[g >>> 5] & (1 << (g & 31))) {
            for (let j = 0; j < nWords; j++) scratch[j] |= guardExps[r + 1 + j];
          }
        }
      }

      for (let w = 0; w < nWords; w++) scratch[nxtBase + w] = 0;

      for (let w = 0; w < nWords; w++) {
        let word = scratch[w];
        while (word !== 0) {
          const off = ctz32(word);
          const s = (w << 5) + off;
          word &= word - 1;
          const word2 = closures[infoOff + s];
          let matched = false;
          switch (word2 & 0xf) {
            case T_BYTE:
              // A literal byte never consumes a separator (§2.2).
              matched = ((word2 >>> 8) & 0xff) === c && !sep;
              break;
            case T_CLASS:
              matched = classMatches(clsRefs[s], c) && !(word2 & dotMaskFlag);
              break;
            case T_ANY_NON_SEP:
              matched = !sep && !(word2 & dotMaskFlag);
              break;
            case T_ANY_BYTE:
              matched = !(word2 & dotMaskFlag);
              break;
            case T_SEP:
              matched = sep;
              break;
          }
          if (matched) {
            const base = (word2 >>> 16) * nWords;
            for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
          }
        }
      }

      let allZero = true;
      for (let w = 0; w < nWords; w++) {
        const v = scratch[nxtBase + w];
        scratch[w] = v;
        if (v !== 0) allZero = false;
      }
      if (allZero) return;
      atSegStart = sep;
    }
  }
}
