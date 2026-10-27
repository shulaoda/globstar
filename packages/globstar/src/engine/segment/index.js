import {
  IS_WINDOWS_SEP,
  endsWithPattern,
  lastSepBefore,
  nextSep,
  hasEscapedSep,
} from "../bytes.js";
import { latin1Bytes, utf8Latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import {
  buildGraph,
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_JOIN,
  EL_PIECE,
  exact,
  nodeWeight,
} from "./graph.js";
import {
  MAX_CHAINS,
  pathsOf,
  chainMatches,
  chainDir,
  hasLiteral,
  R_FIRST,
  R_INNER,
  R_LAST,
  chainIndex,
  probe,
  probeMany,
  probeInfix,
  dirsOf,
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
import { N_BRACE, N_LITERAL, N_SEPARATOR, endsInGlobstar, seq } from "../../parse/ast.js";

export class SegmentMatcher {
  constructor(body, dot, ci, opts) {
    const prefixes = staticPrefixes(body);
    const ends = suffixes(body);
    const graph = buildGraph(body, dot, ci);
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
    this.below = setOnly ? dirsOf(literals) : null;
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
    return this._tryPicked(ix.restLo, ix.restHi, str);
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
    for (let i = 0; i < rest.length; i++) {
      if (ix.stamp[rest[i]] !== ix.epoch) ix.list[ix.listed++] = rest[i];
    }
    return this._tryListed(str);
  }

  _tryListed(str) {
    const ix = this.index;
    while (ix.tried < ix.listed) {
      if (this._picked(ix.list[ix.tried++], str)) return true;
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
      const below = str.length === 0 || hasLiteral(this, this.below, str);
      return DirMatch.from(hasLiteral(this, set, str), below, false);
    }
    if (str.length === 0) {
      let all = false;
      if (this.dot) {
        if (nfa.kind === NFA_SPARSE) all = coversBelowSparse(this, initSparse(nfa));
        else all = coversBelow(this, nfa.init);
      }
      return DirMatch.from(this.isMatch(str), true, all);
    }
    let matches;
    let mayBelow;
    let all;
    if (nfa.kind === NFA_SPARSE) {
      const active = nfaRunSparse(this, str);
      matches = sparseHas(active, nfa.accept);
      mayBelow = sparseReach1(nfa, active);
      all = this.dot && mayBelow && coversBelowSparse(this, active);
    } else {
      const active = nfaRun(this, str);
      matches = (active & (1 << nfa.accept)) !== 0;
      mayBelow = (active & nfa.reach1) !== 0;
      all = this.dot && mayBelow && coversBelow(this, active);
    }
    return DirMatch.from(matches, mayBelow, all);
  }
}

const MAX_SUFFIX_SET = 16;

const MAX_HEAD_DEPTH = 64;
const HEAD_BUDGET_FLOOR = 4096;
const HEAD_COPY_FACTOR = 64;

function suffixes(body) {
  const nodes = seq(body);
  const suffix = trailing(nodes, nodes.length, false);
  if (suffix.length > 0) return [suffix];
  return suffixSet(nodes);
}

function trailing(nodes, end, afterGlobstar) {
  let acc = "";
  for (let i = end - 1; i >= 0; i--) {
    const node = nodes[i];
    if (node.tag === N_LITERAL) acc = node.str + acc;
    else if (node.tag === N_SEPARATOR && !endsInGlobstar(nodes, i, afterGlobstar)) acc = "/" + acc;
    else break;
  }
  return acc;
}

function suffixSet(nodes) {
  if (nodes.length === 0) return [];
  const last = nodes[nodes.length - 1];
  if (last.tag !== N_BRACE) return [];
  const common = trailing(nodes, nodes.length - 1, false);
  const afterGlobstar = endsInGlobstar(nodes, nodes.length - 1, false);
  const many = last.branches.length > MAX_SUFFIX_SET;
  const set = [];
  let shared = null;
  for (const branch of last.branches) {
    const b = seq(branch);
    let own = trailing(b, b.length, afterGlobstar);
    if (!many && b.every((n) => n.tag === N_LITERAL || n.tag === N_SEPARATOR)) own = common + own;
    if (own.length === 0) return [];
    if (!many) {
      set.push(own);
      continue;
    }
    if (shared === null) shared = own;
    else {
      let k = 0;
      while (
        k < shared.length &&
        k < own.length &&
        shared[shared.length - 1 - k] === own[own.length - 1 - k]
      )
        k++;
      shared = shared.slice(shared.length - k);
    }
  }
  if (shared === null) return set;
  return shared.length === 0 ? [] : [shared];
}

function staticPrefixes(body) {
  const nodes = seq(body);
  let heads = readAt(nodes, 0);
  if (heads.copied > HEAD_COPY_FACTOR * heads.limit) heads = readAt(nodes, MAX_HEAD_DEPTH);
  return dedupe(heads.out);
}

function readAt(nodes, depth) {
  const heads = {
    root: nodes,
    spent: 0,
    limit: HEAD_BUDGET_FLOOR,
    weighed: false,
    copied: 0,
    out: [],
  };
  readHeads(heads, nodes, 0, null, "", 0, depth);
  return heads;
}

function readHeads(heads, nodes, at, then, acc, boundary, depth) {
  let i = at;
  let end;
  for (;;) {
    if (i === nodes.length) {
      if (then === null) {
        end = acc.length;
        break;
      }
      nodes = then.nodes;
      i = then.at;
      then = then.next;
      continue;
    }
    const node = nodes[i++];
    if (node.tag === N_LITERAL && hasEscapedSep(node.str)) return;
    if (node.tag === N_LITERAL && afford(heads, node.str.length)) {
      acc += node.str;
    } else if (node.tag === N_SEPARATOR && afford(heads, 1)) {
      acc += "/";
      boundary = acc.length;
    } else if (
      node.tag === N_BRACE &&
      depth < MAX_HEAD_DEPTH &&
      afford(heads, node.branches.length)
    ) {
      const after = { nodes, at: i, next: then };
      for (const branch of node.branches) {
        if (covered(heads)) break;
        readHeads(heads, seq(branch), 0, after, acc, boundary, depth + 1);
      }
      return;
    } else {
      end =
        node.tag === N_BRACE && afford(heads, node.branches.length) && opensSegment(node)
          ? acc.length
          : boundary;
      break;
    }
  }
  while (end > 0 && acc.charCodeAt(end - 1) === 0x2f) end--;
  heads.copied += acc.length;
  if (within(heads, heads.copied, HEAD_COPY_FACTOR)) heads.out.push(acc.slice(0, end));
  else heads.out = [""];
}

const covered = (heads) => heads.out.length > 0 && heads.out[heads.out.length - 1].length === 0;

function afford(heads, n) {
  if (!within(heads, heads.spent + n, 1)) return false;
  heads.spent += n;
  return true;
}

function within(heads, used, factor) {
  if (used > factor * heads.limit && !heads.weighed) {
    heads.weighed = true;
    for (const node of heads.root) heads.limit += 2 * nodeWeight(node);
  }
  return used <= factor * heads.limit;
}

function opensSegment(node) {
  return node.branches.every((b) => seq(b)[0]?.tag === N_SEPARATOR);
}

function dedupe(prefixes) {
  if (prefixes.length <= 1) return prefixes;
  // In byte order a head's prefixes come before it: `top` walks down the
  // kept ones that are prefixes of the current head (`up` links each to
  // the one below), and the first alone may cover it, since one below
  // would have covered that one.
  prefixes.sort();
  if (prefixes[0].length === 0) return [""];
  const kept = [];
  const up = [];
  let top = -1;
  for (let i = 0; i < prefixes.length; i++) {
    const p = prefixes[i];
    while (top !== -1 && !p.startsWith(kept[top])) top = up[top];
    if (top !== -1 && (p.length === kept[top].length || p.charCodeAt(kept[top].length) === 0x2f)) {
      continue;
    }
    up.push(top);
    top = kept.length;
    kept.push(p);
  }
  // Stable: a length's heads stay in byte order.
  return kept.sort((a, b) => a.length - b.length);
}
