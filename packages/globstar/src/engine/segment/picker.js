import { EL_LIT, EL_WILD, room } from "./graph.js";
import { ANY, CLASS, WK_AFFIX, WK_PIECES, WK_SUFFIXES } from "./wild.js";
import { ctz32, fold } from "../bytes.js";

export const MAX_CHAINS = 64;

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

const strings = (kind, keys) => ({ keys: keys.map((k) => [kind, k]), infix: null });

function keysOf(w, ci) {
  const suffixes = [];
  const prefixes = [];
  const exact = [];
  const fixed = !w.variable;
  let pieces = null;
  if (w.kind === WK_AFFIX) {
    prefixes.push(keyOf(w.prefixStr, ci));
    suffixes.push(keyOf(w.suffixStr, ci));
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
    if (fixed && whole) for (const s of suffixes) exact.push(s);
  } else return null;
  if (exact.length > 0) return strings(K_EXACT, exact);
  const s = shortest(suffixes);
  const p = shortest(prefixes);
  if (s === 0 && p === 0) {
    const lit = pieces === null ? null : longestLiteral(pieces);
    return lit === null ? null : { keys: null, infix: lit.slice(0, MAX_INFIX) };
  }
  return p >= s ? strings(K_PREFIX, prefixes) : strings(K_SUFFIX, suffixes);
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
  const idBits = ids > MAX_CHAINS ? 32 - Math.clz32(ids) : 6;
  const many = idBits > 6;
  const ix = {
    slots: null,
    bits: 0,
    idBits,
    lookups: [0, 0, 0],
    sketchLo: new Int32Array(3 * LOOKUPS),
    sketchHi: new Int32Array(3 * LOOKUPS),
    infix: [],
    rest: [],
    picked: new Int32Array(2),
    list: many ? new Int32Array(1 << idBits) : null,
    listed: 0,
    tried: 0,
    stamp: many ? new Int32Array(1 << idBits) : null,
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
    if (!filed) ix.rest.push(id);
    if (entries.length > room(graph.weight)) return null;
  }
  entries.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  dedup(entries);
  if (many) {
    if (ix.rest.length + ix.infix.length > MAX_CHAINS) return null;
    const nfa = (1 << idBits) - 1;
    for (let i = 0, j = 0; i < entries.length; i = j) {
      while (j < entries.length && entries[j][0] === entries[i][0]) j++;
      if (j - i > MAX_CHAINS) for (let k = i; k < j; k++) entries[k][1] = nfa;
    }
    dedup(entries);
  }
  let bits = 1;
  while (1 << bits < 2 * entries.length) bits++;
  ix.bits = bits;
  const slots = new Uint32Array(1 << bits);
  const last = slots.length - 1;
  for (const [key, c] of entries) {
    const h = hashKey(Math.floor(key / 2 ** 32), key >>> 0);
    let at = h >>> (32 - bits);
    while (slots[at] !== 0) at = (at + 1) & last;
    slots[at] = ((tagOf(h, idBits) << idBits) | c) >>> 0;
  }
  ix.slots = slots;
  return ix;
}

function dedup(entries) {
  let n = 0;
  for (const e of entries) {
    if (n === 0 || e[0] !== entries[n - 1][0] || e[1] !== entries[n - 1][1]) entries[n++] = e;
  }
  entries.length = n;
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
