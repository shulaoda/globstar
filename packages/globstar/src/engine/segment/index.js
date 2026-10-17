// The segment engine. JS twin of `crates/globstar/src/engine/segment/
// mod.rs`: a pattern becomes a graph of per-segment elements and a path
// is matched segment by segment — anchored compares for a path through
// the graph with at most one globstar, an NFA over the graph for
// everything else.

import { IS_WINDOWS_SEP } from "../../bytes.js";
import { flat, latin1Bytes, utf8Latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import { compileSegment, opsHaveNonAscii } from "./compile.js";
import { computeStaticPrefixes, endsWithSepAware, suffixes } from "./facts.js";
import {
  chainMatches,
  chainDir,
  nfaRun,
  nfaRunSparse,
  initSparse,
  sparseHas,
  sparseReach1,
  coversBelow,
  coversBelowSparse,
} from "./exec.js";
import {
  NFA_SMALL,
  NFA_SPARSE,
  EL_LIT,
  EL_WILD,
  EL_G0,
  WK_AFFIX,
  WK_AFFIX_SET,
  WK_AFFIX_INDEX,
} from "./elem.js";

export class SegmentMatcher {
  constructor(compiled, ops, ci, byteOnly, dot) {
    this.elems = compiled.elems;
    // The paths through the graph, tried in order; none past MAX_CHAINS.
    this.chains = compiled.chains;
    // The NFA over the graph, kept only when the chains cannot serve:
    // none, several (`matchDir` runs them as one), or one with several
    // globstars.
    this.nfa = compiled.nfa;
    this.ci = ci;
    this.dot = dot;
    this.byteOnly = byteOnly;
    this.prefixes = computeStaticPrefixes(ops).map(flat);
    this.factsPlain = !this.ci && !IS_WINDOWS_SEP;
    const suf = suffixes(ops);
    this.suffixes = suf.length > 0 ? suf.map(flat) : null;
  }

  // How compilation laid the pattern out (a test hook, twin of Rust's
  // `Glob::shape`): the elements in order, the chains, the graph NFA's
  // form. It pins the routing, where a slower path would still answer
  // right.
  shape() {
    const elems = this.elems.map((e) => {
      if (e.kind === EL_LIT) return "L";
      if (e.kind === EL_WILD) {
        const k = e.wild.kind;
        if (k === WK_AFFIX) return "W:affix";
        if (k === WK_AFFIX_SET) return "W:set";
        return k === WK_AFFIX_INDEX ? "W:index" : "W:nfa";
      }
      return e.kind === EL_G0 ? "G0" : "G1";
    });
    // A run of one kind as `L(200)`.
    const runs = [];
    for (let i = 0; i < elems.length; ) {
      let n = 1;
      while (elems[i + n] === elems[i]) n++;
      runs.push(n > 1 ? `${elems[i]}(${n})` : elems[i]);
      i += n;
    }
    const g = this.nfa;
    const nfa = g === null ? "none" : g.kind === NFA_SMALL ? "small" : "sparse";
    return `${runs.join(" ")} | chains ${this.chains.length} | nfa ${nfa}`;
  }

  // `opts.forceNfa` bypasses the chains, `opts.byteOnly` the string
  // mode: test hooks that cross-check the code paths.
  static build(ops, dot, ci, opts = {}) {
    const forceNfa = !!opts.forceNfa;
    const compiled = compileSegment(ops, dot, ci, forceNfa);
    if (forceNfa) compiled.chains = [];
    const byteOnly = !!opts.byteOnly || opsHaveNonAscii(ops);
    return new SegmentMatcher(compiled, ops, ci, byteOnly, dot);
  }

  staticPrefixes() {
    return this.prefixes.map(latin1Bytes);
  }

  // An ASCII pattern matches the path string itself (a wildcard that
  // counts reads a segment's UTF-8 bytes, see `wildConsumes`); one with
  // other bytes matches the path's UTF-8 bytes, one char per byte.
  isMatch(input) {
    return this._isMatch(this.byteOnly ? utf8Latin1(input) : input);
  }

  matchDir(input) {
    return this._matchDir(this.byteOnly ? utf8Latin1(input) : input);
  }

  _isMatch(str) {
    if (!this._factsAccept(str)) return false;
    const chainList = this.chains;
    if (chainList.length === 0) return this._nfaMatch(str);
    if (chainList.length === 1) return chainMatches(this, chainList[0], str);
    // `endsWith` is exact, so the quick suffix is skipped case-insensitively.
    const quick = !this.ci;
    for (const chain of chainList) {
      if (quick && chain.quickSuffixStr.length > 0 && !str.endsWith(chain.quickSuffixStr)) continue;
      if (chainMatches(this, chain, str)) return true;
    }
    return false;
  }

  _nfaMatch(str) {
    const g = this.nfa;
    if (g.kind === NFA_SPARSE) return sparseHas(nfaRunSparse(this, str), g.accept);
    return (nfaRun(this, g, str) & (1 << g.accept)) !== 0;
  }

  _factsAccept(str) {
    const set = this.suffixes;
    if (set === null) return true;
    const plain = this.factsPlain;
    for (let i = 0; i < set.length; i++) {
      if (plain ? str.endsWith(set[i]) : endsWithSepAware(str, set[i], this.ci)) return true;
    }
    return false;
  }

  _matchDir(str) {
    const g = this.nfa;
    if (g === null) return chainDir(this, str);
    if (str.length === 0) {
      // The empty dir is the walk root: nothing consumed yet.
      let all = false;
      if (this.dot) {
        if (g.kind === NFA_SPARSE) all = coversBelowSparse(this, initSparse(g));
        else all = coversBelow(this, g.init);
      }
      return DirMatch.from(this.isMatch(str), true, all);
    }
    let exact;
    let prefix;
    let all;
    if (g.kind === NFA_SPARSE) {
      const active = nfaRunSparse(this, str);
      exact = sparseHas(active, g.accept);
      prefix = sparseReach1(g, active);
      all = this.dot && prefix && coversBelowSparse(this, active);
    } else {
      const active = nfaRun(this, g, str);
      exact = (active & (1 << g.accept)) !== 0;
      prefix = (active & g.reach1) !== 0;
      // `dot=false` never covers a subtree: no wildcard matches the
      // dot-led names below.
      all = this.dot && prefix && coversBelow(this, active);
    }
    return DirMatch.from(exact, prefix, all);
  }
}
