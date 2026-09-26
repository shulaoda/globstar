import { computeStaticPrefixes } from "../ops/index.js";
import { IS_WINDOWS_SEP } from "../../bytes.js";
import { latin1Bytes, utf8Latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import { compileSeqs, opsHaveNonAscii } from "./compile.js";
import { seqMatches, nfaRun, coversBelow, endsWithSepAware } from "./exec.js";

export const MAX_FORKS = 64;
// 31, not Rust's 64: the active set is an int32 bitset and nfaRun/nfaStep
// signal a bail in-band with -1 — bit 31 must stay unallocated or a
// saturated set (0xFFFFFFFF) would read as a bail.
export const MAX_SEQ_STATES = 31;

export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2;
export const EL_G0_STRICT = 3;
export const EL_G1 = 4;

export const WK_AFFIX = 0;
export const WK_AFFIX_SET = 1;
export const WK_GENERIC = 2;

export const NO = 0;
export const YES = 1;
export const BAIL = 2;

// Per-sequence state sets for `matchDir`, shared by every matcher: the
// call is synchronous, and a per-matcher array would cost each one a
// typed-array header.
const ACTIVE = new Int32Array(MAX_FORKS);

export class SegmentMatcher {
  constructor(seqs, program, byteOnly, dot) {
    this.seqs = seqs;
    this.ci = !!program.caseInsensitive;
    this.dot = dot;
    this.byteOnly = byteOnly;
    this.prefixes = computeStaticPrefixes(program.ops);
    this.factsPlain = !this.ci && !IS_WINDOWS_SEP;
    const f = program.facts;
    this.factsSuffixStr = f.suffix.length > 0 ? f.suffix : null;
    this.factsSuffixSetStr = f.suffixSet.length > 0 ? f.suffixSet : null;
  }

  static build(program, dot) {
    const seqs = compileSeqs(program.ops, dot, !!program.caseInsensitive);
    if (seqs === null) return null;
    return new SegmentMatcher(seqs, program, opsHaveNonAscii(program.ops), dot);
  }

  staticPrefixes() {
    return this.prefixes.map(latin1Bytes);
  }

  isMatch(input) {
    if (!this.byteOnly) {
      const r = this._isMatch(input, true);
      if (r !== BAIL) return r === YES;
    }
    return this._isMatch(utf8Latin1(input), false) === YES;
  }

  matchDir(input) {
    if (input.length === 0) {
      // The empty dir is the walk root: nothing consumed yet.
      const seqs = this.seqs;
      for (let i = 0; i < seqs.length; i++) ACTIVE[i] = seqs[i].eps[0];
      const all = this.dot && coversBelow(seqs, ACTIVE);
      return DirMatch.fromExactPrefixAll(this.isMatch(input), true, all);
    }
    if (!this.byteOnly) {
      const r = this._matchDir(input, true);
      if (r !== -1) return r;
    }
    return this._matchDir(utf8Latin1(input), false);
  }

  _isMatch(str, bail) {
    if (!this._factsAccept(str)) return NO;
    const seqs = this.seqs;
    const quick = seqs.length > 1 && !this.ci;
    let bailed = false;
    for (let i = 0; i < seqs.length; i++) {
      const seq = seqs[i];
      if (quick && seq.quickSuffixStr.length > 0 && !str.endsWith(seq.quickSuffixStr)) continue;
      const r = seqMatches(seq, str, this.dot, this.ci, bail);
      if (r === YES) return YES;
      if (r === BAIL) bailed = true;
    }
    return bailed ? BAIL : NO;
  }

  _factsAccept(str) {
    const plain = this.factsPlain;
    const suf = this.factsSuffixStr;
    if (suf !== null) {
      return plain ? str.endsWith(suf) : endsWithSepAware(str, suf, this.ci);
    }
    const set = this.factsSuffixSetStr;
    if (set !== null) {
      for (let i = 0; i < set.length; i++) {
        if (plain ? str.endsWith(set[i]) : endsWithSepAware(str, set[i], this.ci)) return true;
      }
      return false;
    }
    return true;
  }

  _matchDir(str, bail) {
    let exact = false;
    let prefix = false;
    const seqs = this.seqs;
    for (let i = 0; i < seqs.length; i++) {
      const a = nfaRun(seqs[i], str, this.dot, this.ci, bail);
      if (a === -1) return -1;
      ACTIVE[i] = a;
      if ((a & (1 << (seqs[i].numStates - 1))) !== 0) exact = true;
      if ((a & seqs[i].reach1) !== 0) prefix = true;
      // Only the all-below bit needs every sequence's state set.
      if (exact && prefix && !this.dot) break;
    }
    // `dot=false` never covers a subtree: no wildcard matches the
    // dot-led names below.
    const all = this.dot && prefix && coversBelow(seqs, ACTIVE);
    return DirMatch.fromExactPrefixAll(exact, prefix, all);
  }
}
