import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_JOIN,
  elemAnySeg,
  elemSatisfiable,
  exact,
  isGlobstar,
  ACCEPT,
} from "./graph.js";
import { ANY, CLASS, WK_AFFIX, WK_PIECES, WK_SUFFIXES, wildConsumes, wildWeight } from "./wild.js";
import {
  IS_WINDOWS_SEP,
  affixEq,
  asciiLower,
  cmpFolded,
  ctz32,
  isPathSep,
  litEq,
  nextSep,
  lastSepBefore,
  fold,
} from "../bytes.js";
import { DirMatch, PRUNED } from "../../dir-match.js";

export const MAX_CHAINS = 64;

const MAX_LITERAL_BYTES = 64 * 1024;

const NO_G = -1;

const MAX_TAIL_CHECKS = 4096;

const elemAt = (elems, ids, i) => (ids === null ? elems[i] : elems[ids[i]]);
const chainLen = (elems, ids) => (ids === null ? elems.length : ids.length);

export function pathsOf(graph, ci) {
  const none = { literals: null, chains: [] };
  if (graph.glue !== null) return none;
  if (graph.linear) {
    const chain = chainOf(graph, null, true, ci);
    return chain === null ? none : { literals: null, chains: [chain] };
  }
  const paths = [];
  let pathWeight = 0;
  const literalPaths = [];
  let literalChars = 0;
  const path = [];
  const stack = [graph.start];
  const nextAt = [0];
  while (stack.length > 0) {
    const top = stack.length - 1;
    if (nextAt[top] === stack[top].length) {
      stack.pop();
      nextAt.pop();
      path.pop();
      continue;
    }
    const id = stack[top][nextAt[top]++];
    if (id === ACCEPT) {
      const nodes = path.filter((i) => graph.elems[i].kind !== EL_JOIN);
      if (nodes.every((i) => graph.elems[i].kind === EL_LIT)) {
        const joined = nodes.map((i) => graph.elems[i].litStr).join("/");
        literalPaths.push(ci ? asciiLower(joined) : joined);
        literalChars += joined.length;
      } else {
        // A node shared by several paths weighs in each.
        for (const i of nodes) {
          const e = graph.elems[i];
          pathWeight += e.kind === EL_WILD ? wildWeight(e.wild) : 1;
        }
        paths.push(exact(nodes));
      }
      if (
        (paths.length > MAX_CHAINS && pathWeight > 4 * graph.weight) ||
        literalChars + literalPaths.length > Math.max(MAX_LITERAL_BYTES, 4 * graph.weight)
      ) {
        return none;
      }
      continue;
    }
    path.push(id);
    stack.push(graph.succs[id]);
    nextAt.push(0);
  }
  const lone = paths.length === 1 && literalPaths.length === 0;
  const chains = [];
  for (const nodes of paths) {
    const chain = chainOf(graph, nodes, lone, ci);
    if (chain === null) return none;
    chains.push(chain);
  }
  let literals = null;
  if (literalPaths.length > 0) {
    literalPaths.sort();
    literals = literalPaths.filter((s, i) => i === 0 || s !== literalPaths[i - 1]);
  }
  return { literals, chains };
}

function chainOf(graph, nodes, lone, ci) {
  const elems = graph.elems;
  const n = chainLen(elems, nodes);
  const elem = (i) => elemAt(elems, nodes, i);
  let globstar = NO_G;
  let g2 = -1;
  for (let i = 0; i < n; i++) {
    if (!isGlobstar(elem(i))) continue;
    if (globstar === NO_G) globstar = i;
    else if (g2 < 0 && i > globstar + 1) g2 = i;
    else return null;
  }
  if (lone) {
    const firstFrom = (ok) => {
      let i = n;
      while (i > 0 && ok(elem(i - 1))) i--;
      return i;
    };
    return {
      nodes,
      globstar,
      g2,
      openFrom: firstFrom(elemSatisfiable),
      anyFrom: firstFrom(elemAnySeg),
    };
  }
  const first = elem(0);
  const last = elem(n - 1);
  const gateHead = !ci && first.kind === EL_LIT ? first.litStr.charCodeAt(0) : -1;
  let gateSuffix = "";
  let gatePrefix = "";
  let gateLastLo = -1;
  let gateLastHi = -1;
  if (!ci && last.kind === EL_LIT) gateSuffix = last.litStr;
  else if (!ci && last.kind === EL_WILD) {
    const w = last.wild;
    if (w.kind === WK_AFFIX) {
      if (w.suffixStr.length > 0) gateSuffix = w.suffixStr;
      else gatePrefix = w.prefixStr;
    } else if (w.kind === WK_SUFFIXES && !w.suffixEmpty) {
      gateLastLo = 0;
      gateLastHi = 0;
      for (const s of w.suffixSetStr) {
        const c = s.charCodeAt(s.length - 1) & 63;
        if (c < 32) gateLastLo |= 1 << c;
        else gateLastHi |= 1 << (c - 32);
      }
    }
  }
  return { nodes, globstar, g2, gateHead, gateSuffix, gatePrefix, gateLastLo, gateLastHi };
}

export function chainMatches(m, chain, str) {
  const ids = chain.nodes;
  switch (chain.globstar) {
    case NO_G:
      return fixedRun(m, ids, 0, chainLen(m.elems, ids), str, 0);
    default: {
      const g = chain.globstar;
      const midStart = run(m, ids, 0, g, str, 0);
      if (midStart < 0) return false;
      if (chain.g2 < 0) return tailAndGlobstar(m, ids, g, str, midStart);
      const split = tailSplit(m, ids, chain.g2, str, midStart);
      return split >= 0 && blockBetween(m, ids, g, chain.g2, str, midStart, split);
    }
  }
}

export function hasLiteral(m, literals, str) {
  const plain = !m.ci && !IS_WINDOWS_SEP;
  let lo = 0;
  let hi = literals.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const lit = literals[mid];
    const d = plain
      ? lit === str
        ? 0
        : lit < str
          ? -1
          : 1
      : cmpFolded(lit, str, 0, str.length, m.ci);
    if (d === 0) return true;
    if (d < 0) lo = mid + 1;
    else hi = mid;
  }
  return false;
}

export function dirsOf(literals) {
  const dirs = new Set();
  for (const path of literals) {
    for (let i = path.indexOf("/"); i >= 0; i = path.indexOf("/", i + 1))
      dirs.add(path.slice(0, i));
  }
  return [...dirs].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}

function consumeAt(m, e, str, pos, last) {
  if (e.kind === EL_LIT) {
    const end = pos + e.litStr.length;
    const bounded = last ? end === str.length : end < str.length && isPathSep(str.charCodeAt(end));
    return bounded && affixEq(e.litStr, str, pos, m.ci) ? end : -1;
  }
  if (e.kind !== EL_WILD) return -1;
  let end = nextSep(str, pos);
  if (end === -1) end = str.length;
  if ((end === str.length) !== last) return -1;
  return wildConsumes(m, e.wild, str, pos, end) ? end : -1;
}

function elemConsumes(m, e, str, s, t) {
  if (e.kind === EL_LIT) return litEq(e.litStr, str, s, t, m.ci);
  if (e.kind === EL_WILD) return wildConsumes(m, e.wild, str, s, t);
  return false;
}

function fixedRun(m, ids, from, to, str, pos) {
  pos = run(m, ids, from, to - 1, str, pos);
  return pos >= 0 && consumeAt(m, elemAt(m.elems, ids, to - 1), str, pos, true) >= 0;
}

function run(m, ids, from, to, str, pos) {
  const elems = m.elems;
  for (let i = from; i < to; i++) {
    const end = consumeAt(m, elemAt(elems, ids, i), str, pos, false);
    if (end < 0) return -1;
    pos = end + 1;
  }
  return pos;
}

function tailAndGlobstar(m, ids, g, str, midStart) {
  const split = tailSplit(m, ids, g, str, midStart);
  return split >= 0 && absorbs(m, elemAt(m.elems, ids, g).kind, str, midStart, split);
}

function tailSplit(m, ids, g, str, midStart) {
  const elems = m.elems;
  const n = chainLen(elems, ids);
  let split = str.length + 1;
  let end = str.length;
  for (let j = n - 1; j > g; j--) {
    const s = lastSepBefore(str, end) + 1;
    if (s < midStart || !elemConsumes(m, elemAt(elems, ids, j), str, s, end)) return -1;
    split = s;
    if (j > g + 1) {
      if (s === 0) return -1;
      end = s - 1;
    }
  }
  return split;
}

function blockBetween(m, ids, g1, g2, str, midStart, split) {
  if (split === midStart) return false;
  const midEnd = split - 1;
  const elems = m.elems;
  let dotAt = 0;
  let pos = midStart;
  for (;;) {
    let end = nextSep(str, pos);
    if (end === -1) end = str.length;
    const blockEnd = blockFrom(m, ids, g1 + 1, g2, str, pos, end, midEnd);
    if (blockEnd >= 0) {
      const leftAbsorbed = pos > midStart || elemAt(elems, ids, g1).kind === EL_G0;
      const after = blockEnd + 1;
      if (leftAbsorbed) {
        if (dotAt < after) dotAt = firstDot(m, str, after, midEnd);
        if (absorbs(m, elemAt(elems, ids, g2).kind, str, after, split, dotAt)) return true;
      }
    }
    if (end >= midEnd || (!m.dot && pos < end && str.charCodeAt(pos) === 0x2e)) return false;
    pos = end + 1;
  }
}

function blockFrom(m, ids, from, to, str, pos, end, midEnd) {
  const elems = m.elems;
  if (!elemConsumes(m, elemAt(elems, ids, from), str, pos, end)) return -1;
  if (to - from === 1) return end;
  if (end >= midEnd) return -1;
  const p = run(m, ids, from + 1, to - 1, str, end + 1);
  if (p < 0) return -1;
  const last = elemAt(elems, ids, to - 1);
  const taken = consumeAt(m, last, str, p, false);
  if (taken >= 0) return taken <= midEnd ? taken : -1;
  return midEnd === str.length ? consumeAt(m, last, str, p, true) : -1;
}

function absorbs(m, kind, str, from, split, dotAt) {
  if (split === from) return kind === EL_G0;
  const to = split - 1;
  return m.dot || (dotAt === undefined ? firstDot(m, str, from, to) : dotAt) >= to;
}

function firstDot(m, str, from, to) {
  if (m.dot) return Infinity;
  if (from < to && str.charCodeAt(from) === 0x2e) return from;
  for (let i = from; ; i++) {
    i = nextSep(str, i);
    if (i === -1 || i + 1 >= to) return Infinity;
    if (str.charCodeAt(i + 1) === 0x2e) return i + 1;
  }
}

export function chainDir(m, str) {
  const elems = m.elems;
  const chain = m.chains[0];
  const ids = chain.nodes;
  const n = chainLen(elems, ids);
  const g = chain.globstar === NO_G ? n : chain.globstar;
  const tailLen = g < n ? n - g - 1 : 0;
  const openFrom = chain.openFrom;
  const tailAny = n - tailLen >= chain.anyFrom;
  if (str.length === 0) {
    const all = m.dot && g === 0 && tailLen <= 1 && tailAny;
    return DirMatch.from(m.isMatch(str), true, all);
  }
  let midStart = 0;
  for (let i = 0; i < g; i++) {
    const e = elemAt(elems, ids, i);
    let end = consumeAt(m, e, str, midStart, false);
    if (end < 0) {
      end = consumeAt(m, e, str, midStart, true);
      if (end < 0) return PRUNED;
      const k = i + 1;
      const all = m.dot && g < n && k === g && tailLen <= 1 && tailAny;
      return DirMatch.from(k === n, k < n && k >= openFrom, all);
    }
    midStart = end + 1;
  }
  if (g === n) return PRUNED;
  const kind = elemAt(elems, ids, g).kind;
  const matched = tailAndGlobstar(m, ids, g, str, midStart);
  const globstarAlive = absorbs(m, kind, str, midStart, str.length + 1);
  let mayBelow = globstarAlive && g + 1 >= openFrom;
  let all = m.dot && globstarAlive && tailAny;
  if ((!mayBelow || all) && tailLen > 1) {
    const dotAt = firstDot(m, str, midStart, str.length);
    let checks = MAX_TAIL_CHECKS;
    let end = str.length;
    for (let j = 1; j < tailLen; j++) {
      if (checks < j) {
        mayBelow = true;
        all = false;
        break;
      }
      checks -= j;
      const start = lastSepBefore(str, end) + 1;
      const alive =
        absorbs(m, kind, str, midStart, start, dotAt) &&
        fixedRun(m, ids, g + 1, g + 1 + j, str, start);
      mayBelow ||= alive && g + 1 + j >= openFrom;
      all &&= alive;
      if (start === midStart) {
        all &&= j + 1 === tailLen;
        break;
      }
      if (mayBelow && !all) break;
      end = start - 1;
    }
  }
  return DirMatch.from(matched, mayBelow, all);
}

const LINEAR_CHAINS = 10;

const MAX_KEY_PRODUCT = 64;

const KEY_BYTES = 4;

const BASE = 0x01000193;

const MAX_INFIX = 16;

const K_EXACT = 0;
const K_SUFFIX = 1;
const K_PREFIX = 2;

export const R_LAST = 0;
export const R_FIRST = 1;
export const R_INNER = 2;

const LOOKUPS = 1 + 2 * KEY_BYTES;

const EMPTY_KEY = { len: 0, back: 0, front: 0, whole: 0, pow: 1 };

function keyRoom(weight) {
  return Math.max(weight, 4096);
}

function keyOf(str, ci) {
  let pow = 1;
  for (let i = 0; i < str.length; i++) pow = Math.imul(pow, BASE);
  return {
    len: str.length,
    back: packBack(str, 0, str.length, ci),
    front: packFront(str, 0, str.length, ci),
    whole: wholeOf(str, 0, str.length, ci),
    pow: pow >>> 0,
  };
}

function then(a, b) {
  const back = b.len >= KEY_BYTES ? b.back : (b.back | (a.back << (8 * b.len))) >>> 0;
  const front = a.len >= KEY_BYTES ? a.front : (a.front | (b.front << (8 * a.len))) >>> 0;
  return {
    len: a.len + b.len,
    back,
    front,
    whole: (Math.imul(a.whole, b.pow) + b.whole) >>> 0,
    pow: Math.imul(a.pow, b.pow) >>> 0,
  };
}

function shortest(keys) {
  let min = Infinity;
  for (const k of keys) min = Math.min(min, k.len);
  return keys.length === 0 ? 0 : min;
}

function strings(kind, keys) {
  for (const k of keys) if (k.len === 0) return null;
  return { keys: keys.map((k) => [kind, k]), infix: null };
}

function keysOf(w, ci) {
  const suffixes = [];
  const prefixes = [];
  const exact = [];
  const fixed = !w.variable;
  let pieces = null;
  if (w.kind === WK_AFFIX) {
    prefixes.push(keyOf(w.prefixStr, ci));
    suffixes.push(keyOf(w.suffixStr, ci));
    if (fixed && w.minLen === w.prefixStr.length + w.suffixStr.length) {
      exact.push(then(prefixes[0], suffixes[0]));
    }
  } else if (w.kind === WK_SUFFIXES) {
    const p = keyOf(w.prefixStr, ci);
    prefixes.push(p);
    if (!w.suffixEmpty) {
      for (const s of w.suffixSetStr) suffixes.push(keyOf(s, ci));
      if (fixed && w.minLen === w.prefixStr.length)
        for (const s of suffixes) exact.push(then(p, s));
    }
  } else if (w.kind === WK_PIECES) {
    pieces = w.matcher;
    const whole = pieceKeys(pieces, true, ci, suffixes);
    pieceKeys(pieces, false, ci, prefixes);
    if (fixed && whole && pieces.ends.length === 1) for (const s of suffixes) exact.push(s);
  } else return null;
  if (exact.length > 0) return strings(K_EXACT, exact);
  const s = shortest(suffixes);
  const p = shortest(prefixes);
  if (s === 0 && p === 0) {
    const lit = pieces === null ? null : longestLiteral(pieces);
    return lit === null ? null : { keys: null, infix: lit.slice(0, MAX_INFIX) };
  }
  return p > s ? strings(K_PREFIX, prefixes) : strings(K_SUFFIX, suffixes);
}

function pieceKeys(p, back, ci, out) {
  const k = back ? p.ends.length - 1 : 0;
  const from = k === 0 ? 0 : p.ends[k - 1];
  const to = p.ends[k];
  const join = back ? (key, next) => then(next, key) : then;
  out.length = 0;
  out.push(EMPTY_KEY);
  for (let i = from; i < to; i++) {
    const pos = p.at[back ? to - 1 - (i - from) : i];
    if (pos === ANY) return false;
    if (pos < CLASS) {
      const key = keyOf(String.fromCharCode(pos), ci);
      for (let j = 0; j < out.length; j++) out[j] = join(out[j], key);
      continue;
    }
    const set = p.classes[pos - CLASS];
    if (set.neg || (set.bits[4] | set.bits[5] | set.bits[6] | set.bits[7]) !== 0) return false;
    const members = [];
    for (let w = 0; w < 4; w++) {
      for (let word = set.bits[w]; word !== 0; word &= word - 1) {
        const b = (w << 5) | ctz32(word);
        if (members.length === MAX_KEY_PRODUCT) return false;
        if (!(ci && b >= 0x41 && b <= 0x5a)) members.push(keyOf(String.fromCharCode(b), ci));
      }
    }
    if (out.length * members.length > MAX_KEY_PRODUCT) return false;
    const prev = out.slice();
    out.length = 0;
    for (const o of prev) for (const m of members) out.push(join(o, m));
  }
  return true;
}

function longestLiteral(p) {
  let best = "";
  let run = "";
  let from = 0;
  for (const to of p.ends) {
    for (let i = from; i < to; i++) {
      const pos = p.at[i];
      if (pos < ANY) run += String.fromCharCode(pos);
      else {
        if (run.length > best.length) best = run;
        run = "";
      }
    }
    if (run.length > best.length) best = run;
    run = "";
    from = to;
  }
  return best === "" ? null : best;
}

function anchors(chain) {
  const ids = chain.nodes;
  const n = ids.length;
  const g = chain.globstar;
  let head = g < 0 ? n : g;
  let tail = g < 0 ? n : Math.min(g + 1, n);
  let block = -1;
  if (chain.g2 >= 0) {
    tail = chain.g2 + 1;
    block = g + 1;
  }
  const out = [];
  if (tail < n || (head === n && n > 0)) out.push([R_LAST, ids[n - 1]]);
  if (head > 0) out.push([R_FIRST, ids[0]]);
  if (block >= 0) out.push([R_INNER, ids[block]]);
  return out;
}

export function chainIndex(literals, chains, graph, ci) {
  if (chains.length <= LINEAR_CHAINS) return null;
  const elems = graph.elems;
  const firstChain = literals !== null ? 1 : 0;
  const ids = chains.length + firstChain;
  const idBits = Math.max(6, 32 - Math.clz32(ids - 1));
  const many = idBits > 6;
  const ix = {
    slots: null,
    bits: 0,
    idBits,
    lookups: [0, 0, 0],
    sketchLo: new Int32Array(3 * LOOKUPS),
    sketchHi: new Int32Array(3 * LOOKUPS),
    infix: [],
    restLo: 0,
    restHi: 0,
    rest: [],
    picked: new Int32Array(2),
    list: many ? new Int32Array(ids) : null,
    listed: 0,
    tried: 0,
    stamp: many ? new Int32Array(ids) : null,
    epoch: 0,
  };
  const entries = [];
  if (literals !== null) {
    for (const path of literals) {
      const slash = path.indexOf("/");
      const key = keyOf(slash < 0 ? path : path.slice(0, slash), ci);
      file(ix, R_FIRST, K_EXACT, key, 0, entries);
    }
  }
  const read = (node) => {
    const e = elems[node];
    if (e.kind === EL_LIT && e.litStr.length > 0) {
      return { keys: [[K_EXACT, keyOf(e.litStr, ci)]], infix: null };
    }
    return e.kind === EL_WILD ? keysOf(e.wild, ci) : null;
  };
  // Chains that share a node read its keys once.
  const nodeKeys = new Map();
  for (let c = 0; c < chains.length; c++) {
    const id = c + firstChain;
    let filed = false;
    for (const [role, node] of anchors(chains[c])) {
      let k = nodeKeys.get(node);
      if (k === undefined) {
        k = read(node);
        nodeKeys.set(node, k);
      }
      if (k !== null && k.infix === null) {
        for (const [kind, key] of k.keys) file(ix, role, kind, key, id, entries);
        filed = true;
      } else if (k !== null && role === R_LAST) {
        ix.infix.push([id, k.infix]);
        filed = true;
      }
      if (filed) break;
    }
    if (!filed) {
      if (many) ix.rest.push(id);
      else if (id < 32) ix.restLo |= 1 << id;
      else ix.restHi |= 1 << (id - 32);
    }
    if (entries.length > keyRoom(graph.weight)) return null;
  }
  entries.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  if (many) {
    if (ix.rest.length + ix.infix.length > MAX_CHAINS) return null;
    let group = 0;
    for (let i = 0; i < entries.length; i++) {
      const [key, c] = entries[i];
      if (i > 0 && entries[i - 1][0] === key) {
        if (entries[i - 1][1] !== c && ++group > MAX_CHAINS) return null;
      } else group = 1;
    }
  }
  let bits = 1;
  while (1 << bits < 2 * entries.length) bits++;
  ix.bits = bits;
  const slots = new Uint32Array(1 << bits);
  const last = slots.length - 1;
  for (let i = 0; i < entries.length; i++) {
    const [key, c] = entries[i];
    if (i > 0 && entries[i - 1][0] === key && entries[i - 1][1] === c) continue;
    const h = hashKey(Math.floor(key / 2 ** 32), key >>> 0);
    let at = h >>> (32 - bits);
    while (slots[at] !== 0) at = (at + 1) & last;
    slots[at] = ((tagOf(h, idBits) << idBits) | c) >>> 0;
  }
  ix.slots = slots;
  return ix;
}

function file(ix, role, kind, key, c, entries) {
  const short = Math.min(key.len, KEY_BYTES);
  let lookup = 0;
  let len = 0;
  let outer = key.back;
  let cmp = 0;
  if (kind === K_EXACT) {
    len = key.len;
    cmp = ix.stamp !== null ? key.whole : (key.back & mask(key.len)) >>> 0;
  } else {
    if (kind === K_PREFIX) {
      lookup = KEY_BYTES + short;
      outer = key.front;
    } else lookup = short;
    cmp = (outer & mask(short)) >>> 0;
  }
  ix.lookups[role] |= 1 << lookup;
  const bit = outer & 63;
  const at = role * LOOKUPS + lookup;
  if (bit < 32) ix.sketchLo[at] |= 1 << bit;
  else ix.sketchHi[at] |= 1 << (bit - 32);
  entries.push([lookupHi(role, lookup, len) * 2 ** 32 + cmp, c]);
}

const lookupHi = (role, lookup, len) => (role << 16) | (lookup << 8) | Math.min(len, 255);

function hashKey(hi, lo) {
  let h = Math.imul(lo ^ Math.imul(hi, 0x9e3779b1), 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  return h ^ (h >>> 16);
}

function tagOf(h, idBits) {
  const top = 1 << (31 - idBits);
  return ((Math.imul(h, 0x2c1b3c6d) >>> idBits) & (top - 1)) | top;
}

function mark(ix, c) {
  if (ix.stamp === null) {
    if (c < 32) ix.picked[0] |= 1 << c;
    else ix.picked[1] |= 1 << (c - 32);
  } else if (ix.stamp[c] !== ix.epoch) {
    ix.stamp[c] = ix.epoch;
    ix.list[ix.listed++] = c;
  }
}

export function probe(ix, role, str, s, t, ci) {
  const n = t - s;
  let todo = ix.lookups[role];
  const back = packBack(str, s, t, ci);
  const front = todo >>> (KEY_BYTES + 1) !== 0 ? packFront(str, s, t, ci) : 0;
  const slots = ix.slots;
  const last = slots.length - 1;
  const picked = ix.picked;
  while (todo !== 0) {
    const lookup = 31 - Math.clz32(todo & -todo);
    todo &= todo - 1;
    let len = 0;
    let compared = lookup;
    let outer = back;
    if (lookup === 0) {
      len = n;
      compared = n;
    } else if (lookup > KEY_BYTES) {
      compared = lookup - KEY_BYTES;
      outer = front;
    }
    if (compared > n) continue;
    const bit = outer & 63;
    const at = role * LOOKUPS + lookup;
    const sketch = bit < 32 ? ix.sketchLo[at] >>> bit : ix.sketchHi[at] >>> (bit - 32);
    if ((sketch & 1) === 0) continue;
    const h = hashKey(lookupHi(role, lookup, len), (outer & mask(compared)) >>> 0);
    const tag = tagOf(h, 6);
    let i = h >>> (32 - ix.bits);
    for (let slot = slots[i]; slot !== 0; slot = slots[i]) {
      if (slot >>> 6 === tag) {
        const c = slot & 63;
        if (c < 32) picked[0] |= 1 << c;
        else picked[1] |= 1 << (c - 32);
      }
      i = (i + 1) & last;
    }
  }
}

export function probeMany(ix, role, str, s, t, ci) {
  const n = t - s;
  let todo = ix.lookups[role];
  const back = packBack(str, s, t, ci);
  const front = todo >>> (KEY_BYTES + 1) !== 0 ? packFront(str, s, t, ci) : 0;
  const slots = ix.slots;
  const last = slots.length - 1;
  const idBits = ix.idBits;
  const ids = (1 << idBits) - 1;
  const { stamp, epoch, list } = ix;
  while (todo !== 0) {
    const lookup = 31 - Math.clz32(todo & -todo);
    todo &= todo - 1;
    let compared = lookup;
    let outer = back;
    if (lookup === 0) compared = n;
    else if (lookup > KEY_BYTES) {
      compared = lookup - KEY_BYTES;
      outer = front;
    }
    if (compared > n) continue;
    const bit = outer & 63;
    const at = role * LOOKUPS + lookup;
    const sketch = bit < 32 ? ix.sketchLo[at] >>> bit : ix.sketchHi[at] >>> (bit - 32);
    if ((sketch & 1) === 0) continue;
    const h =
      lookup === 0
        ? hashKey(lookupHi(role, 0, n), wholeOf(str, s, t, ci))
        : hashKey(lookupHi(role, lookup, 0), (outer & mask(compared)) >>> 0);
    const tag = tagOf(h, idBits);
    let i = h >>> (32 - ix.bits);
    for (let slot = slots[i]; slot !== 0; slot = slots[i]) {
      const c = slot & ids;
      if (slot >>> idBits === tag && stamp[c] !== epoch) {
        stamp[c] = epoch;
        list[ix.listed++] = c;
      }
      i = (i + 1) & last;
    }
  }
}

export function probeInfix(ix, str, s, t, ci) {
  for (const [c, lit] of ix.infix) if (holds(str, s, t, lit, ci)) mark(ix, c);
}

function holds(str, s, t, lit, ci) {
  if (!ci) {
    const at = str.indexOf(lit, s);
    return at >= 0 && at + lit.length <= t;
  }
  for (let i = s; i + lit.length <= t; i++) {
    let j = 0;
    while (j < lit.length && fold(str.charCodeAt(i + j), true) === lit.charCodeAt(j)) j++;
    if (j === lit.length) return true;
  }
  return false;
}

function packBack(str, s, t, ci) {
  let w = 0;
  if (t - s >= KEY_BYTES) {
    w =
      (str.charCodeAt(t - 1) & 0xff) |
      ((str.charCodeAt(t - 2) & 0xff) << 8) |
      ((str.charCodeAt(t - 3) & 0xff) << 16) |
      ((str.charCodeAt(t - 4) & 0xff) << 24);
  } else {
    for (let i = 0; t - 1 - i >= s; i++) w |= (str.charCodeAt(t - 1 - i) & 0xff) << (8 * i);
  }
  return ci ? foldWord(w) : w >>> 0;
}

function packFront(str, s, t, ci) {
  let w = 0;
  if (t - s >= KEY_BYTES) {
    w =
      (str.charCodeAt(s) & 0xff) |
      ((str.charCodeAt(s + 1) & 0xff) << 8) |
      ((str.charCodeAt(s + 2) & 0xff) << 16) |
      ((str.charCodeAt(s + 3) & 0xff) << 24);
  } else {
    for (let i = 0; s + i < t; i++) w |= (str.charCodeAt(s + i) & 0xff) << (8 * i);
  }
  return ci ? foldWord(w) : w >>> 0;
}

function wholeOf(str, s, t, ci) {
  let h = 0;
  for (let i = s; i < t; i++) h = (Math.imul(h, BASE) + fold(str.charCodeAt(i) & 0xff, ci)) | 0;
  return h >>> 0;
}

function foldWord(w) {
  let out = 0;
  for (let i = 0; i < 32; i += 8) out |= fold((w >>> i) & 0xff, true) << i;
  return out >>> 0;
}

function mask(len) {
  return len >= KEY_BYTES ? 0xffffffff : (1 << (8 * len)) - 1;
}
