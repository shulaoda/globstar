// The segment engine: a pattern as a graph of per-segment elements,
// matched segment by segment. The design and its thresholds:
// `references/theory/03-segment-engine.md`.

import {
  IS_WINDOWS_SEP,
  endsWithPattern,
  lastSepBefore,
  nextSep,
  hasEscapedSep,
} from "../../bytes.js";
import { flat, latin1Bytes, utf8Latin1 } from "../../utf8.js";
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
  // `opts.forceNfa`: no anchored chains (a test hook). `opts.byteOnly`: the
  // path's UTF-8 bytes are matched, not the string (a pattern with a byte
  // past ASCII, and a test hook).
  constructor(body, dot, ci, opts) {
    // The prefixes and the suffixes read the pattern as written; the graph
    // walk regroups it.
    const prefixes = staticPrefixes(body);
    const ends = suffixes(body);
    const graph = buildGraph(body, dot, ci);
    let { literals, chains } = opts.forceNfa ? { literals: null, chains: [] } : pathsOf(graph, ci);
    const index = chainIndex(literals, chains, graph, ci);
    // Past MAX_CHAINS only the index keeps a call's walks few.
    if (index === null && chains.length > MAX_CHAINS) {
      literals = null;
      chains = [];
    }
    const line = literals === null && chains.length === 1 && chains[0].g2 < 0;
    this.elems = graph.elems;
    // The literal paths, searched first; the wildcard paths.
    this.literals = literals;
    this.chains = exact(chains);
    // What a lone line does without: the NFA, and the index of many chains.
    this.nfa = line ? null : anyNfa(graph, ci);
    this.index = index;
    // The chains the current indexed call tried: 0–31, 32–63.
    this.triedLo = 0;
    this.triedHi = 0;
    this.ci = ci;
    this.dot = dot;
    this.byteOnly = opts.byteOnly;
    this.prefixes = prefixes.map(flat);
    this.plain = !this.ci && !IS_WINDOWS_SEP;
    this.suffixes = ends.length > 0 ? ends.map(flat) : null;
  }

  // A test hook: the elements in order, the chains and the NFA's form
  // (`fixtures/engine-shapes.tsv`).
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
    const nfa = g === null ? "none" : g.kind === NFA_SMALL ? "small" : "sparse";
    const literals = this.literals === null ? "" : ` | literals ${this.literals.length}`;
    return `${runs.join(" ")} | chains ${this.chains.length}${literals} | nfa ${nfa}`;
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

  // The literal set first, then each chain after its gate. Its own method:
  // `_isMatch` stays small for the one-chain and NFA matchers.
  _anyChainMatches(chains, str) {
    // In one method with the walk up to 64 chains, the walk past 64 kept
    // `_tryPicked` from being inlined.
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

  // The chains the path's last segment picks first, then its first's, its
  // inner ones', then the chains without a key; each chain once.
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

  // The index names the literal set id 0 and the chains the ids after it,
  // so a step's picks try the set first.
  _picked(id, str) {
    if (this.literals === null) return chainMatches(this, this.chains[id], str);
    if (id === 0) return hasLiteral(this, this.literals, str);
    return chainMatches(this, this.chains[id - 1], str);
  }

  // `_indexedChainMatches` past 64 chains: each step's picks listed once a
  // call, the inner segments' at once.
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
    if (nfa === null) return chainDir(this, str);
    if (str.length === 0) {
      let all = false;
      if (this.dot) {
        if (nfa.kind === NFA_SPARSE) all = coversBelowSparse(this, initSparse(nfa));
        else all = coversBelow(this, nfa.init);
      }
      return DirMatch.from(this.isMatch(str), true, all);
    }
    // `dot=false` never covers a subtree: no wildcard takes the dot-led
    // names below.
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

// The literal ends every match shares, read off the AST: the suffixes
// the prefilter checks and the static prefixes a walker seeds from.

// A pattern that ends in a brace of more branches gets one suffix: the
// one they all share.
const MAX_SUFFIX_SET = 16;

// How deep braces may nest along one head, the budget every pattern gets
// for reading heads, and how many budgets of bytes recording them may cost.
const MAX_HEAD_DEPTH = 64;
const HEAD_BUDGET_FLOOR = 4096;
const HEAD_COPY_FACTOR = 64;

// Literal suffixes one of which every match ends with: the trailing
// literal run, or one per branch of a trailing brace. Empty when nothing
// is guaranteed.
function suffixes(body) {
  const nodes = seq(body);
  const suffix = trailing(nodes, nodes.length, false);
  if (suffix.length > 0) return [suffix];
  return suffixSet(nodes);
}

// The literal/separator run that ends `nodes[..end]`. The `/` of a `**/`
// is not part of it, nor one a brace that may be empty parts from a `**`:
// `**/b` and `**{,a}/b` match `b` alone.
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

// One suffix per branch of a trailing brace, or past MAX_SUFFIX_SET
// branches the suffix the branches' own tails share; none when a branch
// guarantees nothing.
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
    // The common tail joins a branch only when the branch is all literal;
    // otherwise it would claim more than the branch does.
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

// The deepest segment-bounded literal head of each brace expansion,
// deduplicated. Every byte read and branch taken costs one unit of a
// budget; past it, or past MAX_HEAD_DEPTH, a head ends at its last whole
// segment, a shorter prefix that still holds. Past HEAD_COPY_FACTOR
// budgets of bytes copied into heads, the reading starts again at the
// depth limit, which gives the one head before the first brace.
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

// Read `nodes` from `at`, then `then` (`{ nodes, at, next }`), after `acc`
// (whose last whole segment ends at `boundary`), and record the head each
// expansion reaches.
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
    // An expansion through an escaped `\` on Windows matches nothing
    // (GLOB_SPEC §4.2): no head to seed.
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
      // Every branch opening a new segment leaves what came before whole.
      end = node.tag === N_BRACE && opensSegment(node) ? acc.length : boundary;
      break;
    }
  }
  while (end > 0 && acc.charCodeAt(end - 1) === 0x2f) end--;
  heads.copied += acc.length;
  if (within(heads, heads.copied, HEAD_COPY_FACTOR)) heads.out.push(acc.slice(0, end));
  else heads.out = [""];
}

// An empty head covers every other one.
const covered = (heads) => heads.out.length > 0 && heads.out[heads.out.length - 1].length === 0;

function afford(heads, n) {
  if (!within(heads, heads.spent + n, 1)) return false;
  heads.spent += n;
  return true;
}

// The limit is the floor until `used` passes it; the pattern's weight is
// added only then.
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

// Drops the prefixes a shorter one covers; an empty one covers all.
function dedupe(prefixes) {
  if (prefixes.length <= 1) return prefixes;
  prefixes.sort((a, b) => a.length - b.length || (a < b ? -1 : a > b ? 1 : 0));
  const kept = [];
  const coveredBy = (p) => {
    for (let i = p.indexOf("/"); i !== -1; i = p.indexOf("/", i + 1)) {
      let lo = 0;
      let hi = kept.length;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        const k = kept[mid];
        const c = k.length - i || (p.startsWith(k) ? 0 : k < p ? -1 : 1);
        if (c === 0) return true;
        if (c < 0) lo = mid + 1;
        else hi = mid;
      }
    }
    return false;
  };
  for (const p of prefixes) {
    if (p.length === 0) return [p];
    if (p !== kept[kept.length - 1] && !coveredBy(p)) kept.push(p);
  }
  return kept;
}
