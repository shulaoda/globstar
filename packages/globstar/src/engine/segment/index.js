import { IS_WINDOWS_SEP, endsWithPattern, lastSepBefore, nextSep } from "../bytes.js";
import { latin1Bytes, utf8Latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import { buildGraph, EL_LIT, EL_WILD, EL_G0, EL_JOIN, EL_PIECE, exact, survey } from "./graph.js";
import {
  MAX_CHAINS,
  pathsOf,
  chainMatches,
  chainDir,
  hasLiteral,
  anyBelow,
  R_FIRST,
  R_INNER,
  R_LAST,
  chainIndex,
  probe,
  probeMany,
  probeInfix,
} from "./chain.js";
import {
  anyNfa,
  nfaRun,
  nfaRunSparse,
  initSparse,
  sparseHas,
  sparseReach1,
  coversBelow,
  coversBelowSparse,
  NFA_SMALL,
  NFA_SPARSE,
} from "./graph-nfa.js";
import { WK_AFFIX, WK_PIECES, WK_SUFFIXES } from "./wild.js";
import { staticPrefixes, suffixes } from "./ends.js";
import { seq } from "../../parse/ast.js";

export class SegmentMatcher {
  constructor(body, dot, ci, opts) {
    const counts = survey(seq(body));
    const prefixes = staticPrefixes(body, counts.weight);
    const ends = suffixes(body);
    const graph = buildGraph(body, counts, dot, ci);
    let { literals, chains } = opts.forceNfa ? { literals: null, chains: [] } : pathsOf(graph, ci);
    const index = chainIndex(literals, chains, graph, ci);
    if (index === null && chains.length > MAX_CHAINS) {
      literals = null;
      chains = [];
    }
    const line = literals === null && chains.length === 1 && chains[0].g2 < 0;
    const setOnly = literals !== null && chains.length === 0;
    this.elems = setOnly ? [] : graph.elems;
    this.literals = literals;
    this.chains = exact(chains);
    this.nfa = line || setOnly ? null : anyNfa(graph, ci);
    this.index = index;
    this.triedLo = 0;
    this.triedHi = 0;
    this.ci = ci;
    this.dot = dot;
    this.byteOnly = opts.byteOnly;
    this.prefixes = prefixes;
    this.plain = !this.ci && !IS_WINDOWS_SEP;
    this.suffixes = ends.length > 0 ? ends : null;
  }

  shape() {
    const elems = this.elems.map((e) => {
      if (e.kind === EL_LIT) return "L";
      if (e.kind === EL_WILD) {
        const k = e.wild.kind;
        if (k === WK_AFFIX) return "W:affix";
        if (k === WK_PIECES) return "W:pieces";
        return k === WK_SUFFIXES ? "W:set" : "W:nfa";
      }
      if (e.kind === EL_JOIN) return "J";
      if (e.kind === EL_PIECE) return "P";
      return e.kind === EL_G0 ? "G0" : "G1";
    });
    const runs = [];
    for (let i = 0; i < elems.length; ) {
      let n = 1;
      while (elems[i + n] === elems[i]) n++;
      runs.push(n > 1 ? `${elems[i]}(${n})` : elems[i]);
      i += n;
    }
    const g = this.nfa;
    const parts = [];
    if (runs.length > 0) parts.push(runs.join(" "));
    parts.push(`chains ${this.chains.length}`);
    if (this.literals !== null) parts.push(`literals ${this.literals.length}`);
    parts.push(`nfa ${g === null ? "none" : g.kind === NFA_SMALL ? "small" : "sparse"}`);
    return parts.join(" | ");
  }

  staticPrefixes() {
    return this.prefixes.map(latin1Bytes);
  }

  isMatch(input) {
    return this._isMatch(this.byteOnly ? utf8Latin1(input) : input);
  }

  matchDir(input) {
    return this._matchDir(this.byteOnly ? utf8Latin1(input) : input);
  }

  _isMatch(str) {
    if (!this._prefilter(str)) return false;
    const chains = this.chains;
    if (this.literals === null && chains.length <= 1) {
      return chains.length === 0 ? this._nfaMatch(str) : chainMatches(this, chains[0], str);
    }
    return this._anyChainMatches(chains, str);
  }

  _anyChainMatches(chains, str) {
    const ix = this.index;
    if (ix !== null) {
      return ix.stamp === null ? this._indexedChainMatches(str) : this._manyChainMatches(str);
    }
    if (this.literals !== null && hasLiteral(this, this.literals, str)) return true;
    if (chains.length === 0) return false;
    const len = str.length;
    const first = len > 0 ? str.charCodeAt(0) : -1;
    const last = len > 0 ? str.charCodeAt(len - 1) & 63 : 64;
    let lastStart = -1;
    for (let k = 0; k < chains.length; k++) {
      const chain = chains[k];
      const suffix = chain.gateSuffix;
      if (suffix.length > 0) {
        if (!str.endsWith(suffix)) continue;
      } else if (last < 32) {
        if (((chain.gateLastLo >>> last) & 1) === 0) continue;
      } else if (last < 64) {
        if (((chain.gateLastHi >>> (last - 32)) & 1) === 0) continue;
      }
      if (chain.gateHead >= 0 && chain.gateHead !== first) continue;
      const prefix = chain.gatePrefix;
      if (prefix.length > 0) {
        if (lastStart < 0) lastStart = lastSepBefore(str, len) + 1;
        if (!str.startsWith(prefix, lastStart)) continue;
      }
      if (chainMatches(this, chain, str)) return true;
    }
    return false;
  }

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
    const rest = ix.rest;
    for (let i = 0; i < rest.length; i++) if (this._picked(rest[i], str)) return true;
    return false;
  }

  _tryPicked(lo, hi, str) {
    lo &= ~this.triedLo;
    hi &= ~this.triedHi;
    this.triedLo |= lo;
    this.triedHi |= hi;
    while (lo !== 0) {
      const c = 31 - Math.clz32(lo & -lo);
      lo &= lo - 1;
      if (this._picked(c, str)) return true;
    }
    while (hi !== 0) {
      const c = 31 - Math.clz32(hi & -hi);
      hi &= hi - 1;
      if (this._picked(c + 32, str)) return true;
    }
    return false;
  }

  _picked(id, str) {
    if (this.literals === null) return chainMatches(this, this.chains[id], str);
    if (id === 0) return hasLiteral(this, this.literals, str);
    return chainMatches(this, this.chains[id - 1], str);
  }

  _manyChainMatches(str) {
    const ix = this.index;
    const ci = this.ci;
    const len = str.length;
    const lastStart = lastSepBefore(str, len) + 1;
    if (++ix.epoch === 0x7fffffff) {
      ix.stamp.fill(0);
      ix.epoch = 1;
    }
    ix.listed = 0;
    ix.tried = 0;
    probeMany(ix, R_LAST, str, lastStart, len, ci);
    if (ix.infix.length > 0) probeInfix(ix, str, lastStart, len, ci);
    if (this._tryListed(str)) return true;
    const firstEnd = lastStart === 0 ? len : nextSep(str, 0);
    if (ix.lookups[R_FIRST] !== 0) {
      probeMany(ix, R_FIRST, str, 0, firstEnd, ci);
      if (this._tryListed(str)) return true;
    }
    if (ix.lookups[R_INNER] !== 0) {
      for (let s = 0, end = firstEnd; s < lastStart; s = end + 1, end = nextSep(str, s)) {
        probeMany(ix, R_INNER, str, s, end, ci);
      }
    }
    const rest = ix.rest;
    for (let i = 0; i < rest.length; i++) ix.list[ix.listed++] = rest[i];
    const nfaPicked = ix.stamp[(1 << ix.idBits) - 1] === ix.epoch;
    return this._tryListed(str) || (nfaPicked && this._nfaMatch(str));
  }

  _tryListed(str) {
    const ix = this.index;
    const nfa = (1 << ix.idBits) - 1;
    while (ix.tried < ix.listed) {
      const id = ix.list[ix.tried++];
      if (id !== nfa && this._picked(id, str)) return true;
    }
    return false;
  }

  _nfaMatch(str) {
    const nfa = this.nfa;
    if (nfa.kind === NFA_SPARSE) return sparseHas(nfaRunSparse(this, str), nfa.accept);
    return (nfaRun(this, str) & (1 << nfa.accept)) !== 0;
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
    const nfa = this.nfa;
    if (nfa === null) {
      if (this.chains.length > 0) return chainDir(this, str);
      const set = this.literals;
      const below = str.length === 0 || anyBelow(this, set, str);
      return DirMatch.from(hasLiteral(this, set, str), below, false);
    }
    const root = str.length === 0;
    let matches;
    let mayBelow;
    let all;
    if (nfa.kind === NFA_SPARSE) {
      const active = root ? initSparse(nfa) : nfaRunSparse(this, str);
      matches = root ? this.isMatch(str) : sparseHas(active, nfa.accept);
      mayBelow = root || sparseReach1(nfa, active);
      all = this.dot && mayBelow && coversBelowSparse(this, active);
    } else {
      const active = root ? nfa.init : nfaRun(this, str);
      matches = root ? this.isMatch(str) : (active & (1 << nfa.accept)) !== 0;
      mayBelow = root || (active & nfa.reach1) !== 0;
      all = this.dot && mayBelow && coversBelow(this, active);
    }
    return DirMatch.from(matches, mayBelow, all);
  }
}
