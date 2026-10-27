import { IS_WINDOWS_SEP, endsWithPattern, lastSepBefore, nextSep } from "../bytes.js";
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
  room,
  ACCEPT,
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
import { ANY, WK_AFFIX, WK_PIECES, WK_SUFFIXES } from "./wild.js";

export class SegmentMatcher {
  constructor(body, dot, ci, opts) {
    const graph = buildGraph(body, dot, ci);
    const prefixes = staticPrefixes(graph);
    const ends = suffixes(graph);
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

// The literal ends every match shares, read off the graph: the suffixes
// the prefilter checks and the static prefixes a walker seeds from.

// Past this many suffixes the prefilter keeps the one they all share.
const MAX_SUFFIX_SET = 16;

// The nodes after `i`, the start being `elems.length`: how many, and the
// `k`th. A linear graph's next is the node after, then `ACCEPT`.
function nextCount(graph, i) {
  if (i === ACCEPT) return 0;
  return graph.linear ? 1 : i === graph.elems.length ? graph.start.length : graph.succs[i].length;
}

function nextAt(graph, i, k) {
  const n = graph.elems.length;
  if (graph.linear) return i === n ? (n > 0 ? 0 : ACCEPT) : i + 1 < n ? i + 1 : ACCEPT;
  return i === n ? graph.start[k] : graph.succs[i][k];
}

const ONE_END = [""];

// What an element takes when it is a few literal segments: its prefix,
// then one of its ends. A literal segment is its string and one empty end;
// a suffix set without a wildcard is its prefix and its suffixes. `null`
// for any other element.
function exactEnds(e) {
  if (e.kind === EL_LIT) return ONE_END;
  if (e.kind !== EL_WILD) return null;
  const w = e.wild;
  if (w.kind !== WK_SUFFIXES || w.variable || w.minLen !== w.prefixStr.length) return null;
  return w.suffixEmpty ? [...w.suffixSetStr, ""] : w.suffixSetStr;
}

const exactPrefix = (e) => (e.kind === EL_LIT ? e.litStr : e.wild.prefixStr);

// The bytes the last piece ends with, folded when case-insensitive.
function lastBytes(pieces) {
  const { at, ends } = pieces;
  const from = ends.length > 1 ? ends[ends.length - 2] : 0;
  const to = ends[ends.length - 1];
  let i = to;
  while (i > from && at[i - 1] < ANY) i--;
  let out = "";
  for (let k = i; k < to; k++) out += String.fromCharCode(at[k]);
  return out;
}

function sharedSuffix(strings) {
  let shared = strings[0];
  for (const x of strings) {
    let k = 0;
    while (
      k < shared.length &&
      k < x.length &&
      shared[shared.length - 1 - k] === x[x.length - 1 - k]
    ) {
      k++;
    }
    shared = shared.slice(shared.length - k);
  }
  return shared;
}

// Literal suffixes one of which every match ends with, in pattern order;
// empty when nothing is guaranteed. An end that takes whole literal
// segments reads back through the literal segments before it, each the
// one before the next, up to a `**/`, a join or the start; a segment of
// any other kind leaves the `/` before.
function suffixes(graph) {
  const n = graph.elems.length;
  // A node's one predecessor; -1 past the start or a join.
  let pred = null;
  if (!graph.linear) {
    pred = new Int32Array(n).fill(-2);
    for (let p = 0; p < n; p++) {
      for (const t of graph.succs[p]) if (t !== ACCEPT) pred[t] = pred[t] === -2 ? p : -1;
    }
  }
  const set = [];
  // In pattern order, which the prefilter tries first; past
  // `MAX_SUFFIX_SET` the set gives way to what all share.
  const push = (s) => {
    if (set.length > MAX_SUFFIX_SET || !set.includes(s)) set.push(s);
  };
  for (let e = 0; e <= n; e++) {
    let ends = false;
    for (let k = 0, m = nextCount(graph, e); k < m; k++) ends ||= nextAt(graph, e, k) === ACCEPT;
    if (!ends) continue;
    if (e === n) return [];
    const el = graph.elems[e];
    const exact = exactEnds(el);
    if (exact !== null && exact.length <= MAX_SUFFIX_SET) {
      // Past `MAX_SUFFIX_SET` only what all share counts, and the ends
      // alone give that.
      let run = "";
      for (let at = e; set.length <= MAX_SUFFIX_SET; ) {
        const p = pred === null ? at - 1 : pred[at];
        if (p < 0) break;
        const before = graph.elems[p];
        const segs = exactEnds(before);
        if (segs === null || segs.length !== 1) {
          if (before.kind !== EL_G0 && before.kind !== EL_JOIN) run = "/" + run;
          break;
        }
        run = exactPrefix(before) + segs[0] + "/" + run;
        at = p;
      }
      for (const end of exact) push(run + exactPrefix(el) + end);
    } else if (exact !== null) {
      push(sharedSuffix(exact));
    } else if (el.kind === EL_WILD && el.wild.kind === WK_AFFIX) {
      push(el.wild.suffixStr);
    } else if (el.kind === EL_WILD && el.wild.kind === WK_SUFFIXES && !el.wild.suffixEmpty) {
      for (const x of el.wild.suffixSetStr) push(x);
    } else if (el.kind === EL_WILD && el.wild.kind === WK_PIECES) {
      push(lastBytes(el.wild.matcher));
    } else {
      return [];
    }
  }
  const out = set.length > MAX_SUFFIX_SET ? [sharedSuffix(set)] : set;
  return out.some((x) => x.length === 0) ? [] : out;
}

// The deepest segment-bounded literal head of every path, read from the
// start through literal segments, deduplicated; the empty head ends the
// reading, as it covers every other. Every byte a head takes at a node,
// and every node it moves to, costs a unit of the room, paid before the
// head is built; a head the room cannot pay for ends where it is.
function staticPrefixes(graph) {
  const limit = room(graph.weight);
  const nodes = [];
  const heads = [];
  const seen = [];
  for (let k = 0, m = nextCount(graph, graph.elems.length); k < m; k++) {
    nodes.push(nextAt(graph, graph.elems.length, k));
    heads.push("");
    seen.push(false);
  }
  const out = [];
  let spent = 0;
  while (nodes.length > 0) {
    const t = nodes.pop();
    const head = heads.pop();
    const seg = seen.pop();
    const e = t === ACCEPT ? null : graph.elems[t];
    const m = nextCount(graph, t);
    // A join takes nothing and passes the head on.
    const join = e !== null && e.kind === EL_JOIN;
    const ends = e === null ? null : join ? ONE_END : exactEnds(e);
    const prefix = ends === null || join ? "" : exactPrefix(e);
    let cost = 1;
    if (ends !== null)
      for (const end of ends) cost += (head.length + 1 + prefix.length + end.length) * m;
    if (ends === null || spent + cost > limit) {
      let to = head.length;
      while (to > 0 && head.charCodeAt(to - 1) === 0x2f) to--;
      // The empty head covers every other.
      if (to === 0) return [""];
      out.push(head.slice(0, to));
      continue;
    }
    spent += cost;
    const base = join ? head : (seg ? head + "/" : head) + prefix;
    for (const end of ends) {
      for (let k = 0; k < m; k++) {
        nodes.push(nextAt(graph, t, k));
        heads.push(base + end);
        seen.push(seg || !join);
      }
    }
  }
  return dedupe(out);
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
