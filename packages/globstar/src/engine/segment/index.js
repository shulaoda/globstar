// The segment engine. JS twin of `crates/globstar/src/engine/segment/
// mod.rs`: a pattern becomes a graph of per-segment elements and a path
// is matched segment by segment — anchored compares for a path through
// the graph with at most one globstar, an NFA over the graph for
// everything else.

import { computeStaticPrefixes } from "../ops/index.js";
import { IS_WINDOWS_SEP } from "../../bytes.js";
import { latin1Bytes, utf8Latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import { compileSegment, opsHaveNonAscii } from "./compile.js";
import { endsWithSepAware } from "../facts.js";
import { chainMatches, nfaRun, nfaRunWide, coversBelow, coversBelowWide, affixEq } from "./exec.js";
import { bitTest, bitsIntersect } from "./bits.js";

// Paths through the graph `isMatch` still tries one by one; past that
// it runs the NFA over the whole graph, as `matchDir` always does.
export const MAX_CHAINS = 64;
// An affix set up to this size is scanned in pattern order; a larger
// one is sorted and binary-searched per suffix length.
export const LINEAR_SUFFIXES = 8;

export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2;
export const EL_G0_STRICT = 3;
export const EL_G1 = 4;

export const WK_AFFIX = 0;
export const WK_AFFIX_SET = 1;
export const WK_GENERIC = 2;
export const WK_AFFIX_INDEX = 3;

// `chain.globstar` when the chain has none / several.
export const NO_G = -1;
export const MANY_G = -2;

export const NO = 0;
export const YES = 1;
export const BAIL = 2;

export class SegmentMatcher {
  constructor(compiled, program, byteOnly, dot) {
    this.nodes = compiled.elems;
    this.graph = compiled.nfa;
    this.chains = compiled.chains;
    this.ci = !!program.caseInsensitive;
    this.dot = dot;
    this.byteOnly = byteOnly;
    this.prefixes = computeStaticPrefixes(program.ops);
    this.factsPlain = !this.ci && !IS_WINDOWS_SEP;
    const f = program.facts;
    this.factsSuffixStr = f.suffix.length > 0 ? f.suffix : null;
    this.factsSuffixSetStr = f.suffixSet.length > 0 ? f.suffixSet : null;
  }

  // `opts.forceNfa` bypasses the chains, `opts.byteOnly` the string
  // mode: test hooks that cross-check the code paths.
  static build(program, dot, opts = {}) {
    const compiled = compileSegment(program.ops, dot, !!program.caseInsensitive);
    if (opts.forceNfa) compiled.chains = [];
    const byteOnly = !!opts.byteOnly || opsHaveNonAscii(program.ops);
    return new SegmentMatcher(compiled, program, byteOnly, dot);
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
      const g = this.graph;
      const all =
        this.dot && (g.wide ? coversBelowWide(this, g.init.slice()) : coversBelow(this, g.init));
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
    const chainList = this.chains;
    if (chainList.length === 0) return this._nfaMatch(str, bail);
    if (chainList.length === 1) return chainMatches(this, chainList[0], str, bail);
    // `endsWith` is exact, so the quick suffix is skipped case-insensitively.
    const quick = !this.ci;
    let bailed = false;
    for (const chain of chainList) {
      if (quick && chain.quickSuffixStr.length > 0 && !str.endsWith(chain.quickSuffixStr)) continue;
      const r = chainMatches(this, chain, str, bail);
      if (r === YES) return YES;
      if (r === BAIL) bailed = true;
    }
    return bailed ? BAIL : NO;
  }

  _nfaMatch(str, bail) {
    const g = this.graph;
    if (g.wide) {
      const active = nfaRunWide(this, str, bail);
      if (active === null) return BAIL;
      return bitTest(active, g.accept) ? YES : NO;
    }
    const active = nfaRun(this, g, str, bail);
    if (active === -1) return BAIL;
    return (active & (1 << g.accept)) !== 0 ? YES : NO;
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
    const g = this.graph;
    let exact;
    let prefix;
    let all;
    if (g.wide) {
      const active = nfaRunWide(this, str, bail);
      if (active === null) return -1;
      exact = bitTest(active, g.accept);
      prefix = bitsIntersect(active, g.reach1);
      // `dot=false` never covers a subtree: no wildcard matches the
      // dot-led names below.
      all = this.dot && prefix && coversBelowWide(this, active);
    } else {
      const active = nfaRun(this, g, str, bail);
      if (active === -1) return -1;
      exact = (active & (1 << g.accept)) !== 0;
      prefix = (active & g.reach1) !== 0;
      all = this.dot && prefix && coversBelow(this, active);
    }
    return DirMatch.fromExactPrefixAll(exact, prefix, all);
  }
}

export { affixEq };
