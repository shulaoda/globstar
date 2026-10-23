// The segment engine. JS twin of `crates/globstar/src/engine/segment/
// mod.rs`: a pattern becomes a graph of per-segment elements and a path
// is matched segment by segment — anchored compares for a path through
// the graph with at most one globstar or two around a block, one search
// of a sorted set for the literal-only paths, only the chains its
// segments pick for a union of many (`chain-index.js`), an NFA over the
// graph for everything else. A segment a brace cuts is a run of glued
// pieces, which one in-segment NFA matches together (`GlueNfa`). The
// graph is linear in the pattern: every pattern compiles.

import { IS_WINDOWS_SEP, endsWithPattern, lastSepBefore, nextSep } from "../../bytes.js";
import { flat, latin1Bytes, utf8Latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import { buildGraph } from "./graph.js";
import { SET_G, chains, chainMatches, chainDir, hasLiteral } from "./chain.js";
import {
  anyNfa,
  nfaRun,
  nfaRunSparse,
  initSparse,
  sparseHas,
  sparseReach1,
  coversBelow,
  coversBelowSparse,
} from "./nfa.js";
import { computeStaticPrefixes, suffixes } from "./facts.js";
import { R_FIRST, R_INNER, R_LAST, chainIndex, probe, probeInfix } from "./chain-index.js";
import {
  NFA_SMALL,
  NFA_SPARSE,
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_JOIN,
  EL_PIECE,
  WK_AFFIX,
  WK_SUFFIXES,
  exact,
} from "./elem.js";

export class SegmentMatcher {
  // `opts.forceNfa` bypasses the chains (a test hook); `opts.byteOnly`
  // the string mode, for a pattern with a byte past ASCII (and as a
  // test hook that cross-checks the code paths).
  constructor(body, dot, ci, opts) {
    // The facts read the pattern as written; the graph walk regroups it.
    const prefixes = computeStaticPrefixes(body);
    const suf = suffixes(body);
    const graph = buildGraph(body, dot, ci);
    const chainList = opts.forceNfa ? [] : chains(graph, ci);
    // A lone line (one chain through at most one globstar) serves
    // `matchDir` too.
    const line = chainList.length === 1 && chainList[0].globstar !== SET_G && chainList[0].g2 < 0;
    this.elems = graph.elems;
    // The paths through the graph, tried in order (the literal ones as
    // one set, first); none past the chain module's caps or for the
    // test hook.
    this.chains = exact(chainList);
    // What a lone line does without: the NFA over the graph, which
    // serves `matchDir` (and `isMatch` when the chains cannot), and,
    // for many chains, their index.
    this.nfa = line ? null : anyNfa(graph, ci);
    this.index = line ? null : chainIndex(chainList, graph, ci);
    // The chains the current indexed call tried: 0–31, 32–63.
    this.triedLo = 0;
    this.triedHi = 0;
    this.ci = ci;
    this.dot = dot;
    this.byteOnly = opts.byteOnly;
    this.prefixes = prefixes.map(flat);
    this.plain = !this.ci && !IS_WINDOWS_SEP;
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
        return k === WK_SUFFIXES ? "W:set" : "W:nfa";
      }
      if (e.kind === EL_JOIN) return "J";
      if (e.kind === EL_PIECE) return "P";
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
    // The literal paths' set is counted apart from the chains.
    const first = this.chains[0];
    const set = first !== undefined && first.globstar === SET_G;
    const literals = set ? ` | literals ${first.literals.length}` : "";
    return `${runs.join(" ")} | chains ${this.chains.length - (set ? 1 : 0)}${literals} | nfa ${nfa}`;
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
    if (!this._prefilter(str)) return false;
    const chainList = this.chains;
    if (chainList.length === 0) return this._nfaMatch(str);
    if (chainList.length === 1) return chainMatches(this, chainList[0], str);
    return this._anyChainMatches(chainList, str);
  }

  // Does one of the chains match the whole path? Each runs after quick
  // looks at the path's ends: its first character against a leading
  // literal's, its last against a literal suffix, an affix's prefix or a
  // set's last characters (`chainOf`; none when case-insensitive). Its
  // own method, so `_isMatch` stays small for the one-chain and NFA
  // matchers.
  _anyChainMatches(chainList, str) {
    if (this.index !== null) return this._indexedChainMatches(str);
    // The literal paths' set, when there is one, comes first.
    let from = 0;
    if (chainList[0].globstar === SET_G) {
      if (hasLiteral(this, chainList[0].literals, str)) return true;
      from = 1;
    }
    const len = str.length;
    const first = len > 0 ? str.charCodeAt(0) : -1;
    const last = len > 0 ? str.charCodeAt(len - 1) & 63 : 64;
    // Where the last segment starts, found once some chain asks.
    let lastStart = -1;
    for (let k = from; k < chainList.length; k++) {
      const chain = chainList[k];
      const suffix = chain.quickSuffixStr;
      if (suffix.length > 0) {
        if (!str.endsWith(suffix)) continue;
      } else if (last < 32) {
        if (((chain.quickLastLo >>> last) & 1) === 0) continue;
      } else if (last < 64) {
        if (((chain.quickLastHi >>> (last - 32)) & 1) === 0) continue;
      }
      if (chain.quickHead >= 0 && chain.quickHead !== first) continue;
      const prefix = chain.quickPrefixStr;
      if (prefix.length > 0) {
        if (lastStart < 0) lastStart = lastSepBefore(str, len) + 1;
        if (!str.startsWith(prefix, lastStart)) continue;
      }
      if (chainMatches(this, chain, str)) return true;
    }
    return false;
  }

  // `_anyChainMatches` through the chain index: the chains the path's
  // last segment picks first, then those of its first, of its inner
  // ones, then those without a key; each chain once, walked without the
  // quick looks. Most were picked by the segment a quick look reads, and
  // the walk turns the others down about as fast.
  _indexedChainMatches(str) {
    const ix = this.index;
    const ci = this.ci;
    const len = str.length;
    const lastStart = lastSepBefore(str, len) + 1;
    const picked = ix.picked;
    this.triedLo = 0;
    this.triedHi = 0;
    picked[0] = picked[1] = 0;
    probe(ix, R_LAST, str, lastStart, len, ci);
    if (ix.infix.length > 0) probeInfix(ix, str, lastStart, len, ci);
    if (this._tryPicked(picked[0], picked[1], str)) return true;
    // Where the first segment ends; the last one starts after it.
    const firstEnd = lastStart === 0 ? len : nextSep(str, 0);
    if (ix.lookups[R_FIRST] !== 0) {
      picked[0] = picked[1] = 0;
      probe(ix, R_FIRST, str, 0, firstEnd, ci);
      if (this._tryPicked(picked[0], picked[1], str)) return true;
    }
    if (ix.lookups[R_INNER] !== 0) {
      for (let s = 0, end = firstEnd; s < lastStart; s = end + 1, end = nextSep(str, s)) {
        picked[0] = picked[1] = 0;
        probe(ix, R_INNER, str, s, end, ci);
        if (this._tryPicked(picked[0], picked[1], str)) return true;
      }
    }
    return this._tryPicked(ix.restLo, ix.restHi, str);
  }

  // Walk the chains of `lo`/`hi` (bits for chains 0–31, 32–63) not tried
  // yet this call, in chain order.
  _tryPicked(lo, hi, str) {
    lo &= ~this.triedLo;
    hi &= ~this.triedHi;
    this.triedLo |= lo;
    this.triedHi |= hi;
    while (lo !== 0) {
      const c = 31 - Math.clz32(lo & -lo);
      lo &= lo - 1;
      if (chainMatches(this, this.chains[c], str)) return true;
    }
    while (hi !== 0) {
      const c = 31 - Math.clz32(hi & -hi);
      hi &= hi - 1;
      if (chainMatches(this, this.chains[c + 32], str)) return true;
    }
    return false;
  }

  _nfaMatch(str) {
    const g = this.nfa;
    if (g.kind === NFA_SPARSE) return sparseHas(nfaRunSparse(this, str), g.accept);
    return (nfaRun(this, str) & (1 << g.accept)) !== 0;
  }

  _prefilter(str) {
    const set = this.suffixes;
    if (set === null) return true;
    const plain = this.plain;
    for (let i = 0; i < set.length; i++) {
      if (plain ? str.endsWith(set[i]) : endsWithPattern(str, set[i], this.ci)) return true;
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
      const active = nfaRun(this, str);
      exact = (active & (1 << g.accept)) !== 0;
      prefix = (active & g.reach1) !== 0;
      // `dot=false` never covers a subtree: no wildcard matches the
      // dot-led names below.
      all = this.dot && prefix && coversBelow(this, active);
    }
    return DirMatch.from(exact, prefix, all);
  }
}
