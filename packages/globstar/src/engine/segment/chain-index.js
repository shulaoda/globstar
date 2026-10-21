// The chain index: which chains of a big union a path may match, by
// what its segments are, end with, start with or hold. JS twin of
// `crates/globstar/src/engine/segment/chain_index.rs`: each chain is
// filed under the literal strings one of its anchored segments must
// have — its last (a tail), else its first (a head), else a block's
// first — read off that segment's nodes when the graph is built; the
// literal paths' set under their first segments. The matcher looks up
// a path's own last, first and inner segments and tries only the
// chains they pick (`_indexedChainMatches`); the chain walk decides.

import { CI_BYTE, N_BRACE, N_CLASS, N_LITERAL, seq } from "../../parse/ast.js";
import { EL_LIT, EL_WILD } from "./elem.js";
import { fold } from "../../bytes.js";

// A union of at most this many chains (the literal set aside) tries
// them in turn after their gates, which costs less than the lookups.
export const LINEAR_CHAINS = 10;

// A segment's strings multiply past this many no more: a product of
// alternatives (`[._]s[a-v][a-z]`) is cut to the nodes that fit; a
// single list (a merged set of names) is kept whatever its length.
const MAX_KEY_PRODUCT = 64;

// A key compares this many outer characters of a segment at most (the
// last of a suffix, the first of a prefix): the walk checks the rest.
const KEY_BYTES = 4;

// How a segment's strings bind it: every match is one of them, ends
// with one, or starts with one.
const K_EXACT = 0;
const K_SUFFIX = 1;
const K_PREFIX = 2;

// Which segment of a path a chain's key is looked up in: the last (the
// chain's last element takes it), the first (its first element), any
// but the last (a block's first element takes one of them).
export const R_LAST = 0;
export const R_FIRST = 1;
export const R_INNER = 2;

// The lookups of one role: an exact key of the segment's length (0), a
// suffix of each length up to KEY_BYTES (1 to 4), a prefix of each (5
// to 8).
const LOOKUPS = 1 + 2 * KEY_BYTES;

// A string by its length and outer characters, folded when
// case-insensitive: `back` holds its last KEY_BYTES (the last lowest),
// `front` its first (the first lowest).
function keyOf(str, ci) {
  return {
    len: str.length,
    back: packBack(str, 0, str.length, ci),
    front: packFront(str, 0, str.length, ci),
  };
}

const EMPTY_KEY = { len: 0, back: 0, front: 0 };

// `a` then `b`.
function then(a, b) {
  const back = b.len >= KEY_BYTES ? b.back : (b.back | (a.back << (8 * b.len))) >>> 0;
  const front = a.len >= KEY_BYTES ? a.front : (a.front | (b.front << (8 * a.len))) >>> 0;
  return { len: a.len + b.len, back, front };
}

// The keys of a segment's nodes (`oneKind`): `{ keys }`, a list of
// [kind, key], or `{ infix }`. A segment that is one brace its branches
// bind apart — a union's members of one segment, `{Makefile,inc.*,*.pc}`
// — is bound by all their keys, each its own kind; a branch without
// strings leaves it none.
export function segmentKeys(nodes, ci) {
  const keys = oneKind(nodes, ci);
  if (keys !== null || nodes.length !== 1 || nodes[0].tag !== N_BRACE) return keys;
  const out = [];
  for (const branch of nodes[0].branches) {
    const k = segmentKeys(seq(branch), ci);
    if (k === null || k.keys === null) return null;
    for (const kk of k.keys) out.push(kk);
  }
  return { keys: out, infix: null };
}

// One kind of keys for a segment's nodes: its strings when it is
// literal (`.DS_Store`, `[Dd]ebug`, `{dist,out}`); else those of the
// literal nodes that end it or that start it, whichever bind more
// characters; else its longest literal. Null when it has no literal, or
// a string may be empty.
function oneKind(nodes, ci) {
  const suffixes = [];
  const taken = literalRun(nodes, true, ci, suffixes);
  if (taken === nodes.length) return strings(K_EXACT, suffixes);
  const prefixes = [];
  literalRun(nodes, false, ci, prefixes);
  const s = shortest(suffixes);
  const p = shortest(prefixes);
  if (s === 0 && p === 0) {
    let longest = null;
    for (const n of nodes) {
      if (n.tag === N_LITERAL && (longest === null || n.str.length > longest.length))
        longest = n.str;
    }
    if (longest === null) return null;
    let infix = "";
    for (let i = 0; i < longest.length; i++)
      infix += String.fromCharCode(fold(longest.charCodeAt(i), ci));
    return { keys: null, infix };
  }
  return p > s ? strings(K_PREFIX, prefixes) : strings(K_SUFFIX, suffixes);
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

// The strings of the literal nodes at one end of `nodes`, as many as
// fit MAX_KEY_PRODUCT, into `out`; how many nodes they took. `back`:
// from the last node on, each one's strings going in front.
function literalRun(nodes, back, ci, out) {
  let cur = [EMPTY_KEY];
  let taken = 0;
  for (let k = 0; k < nodes.length; k++) {
    const alts = alternatives(nodes[back ? nodes.length - 1 - k : k], ci);
    if (alts === null) break;
    if (alts.length > 1 && cur.length > 1 && cur.length * alts.length > MAX_KEY_PRODUCT) break;
    const next = [];
    for (const o of cur) for (const a of alts) next.push(back ? then(a, o) : then(o, a));
    cur = next;
    taken++;
  }
  for (const k of cur) out.push(k);
  return taken;
}

// The strings one literal node matches: a literal, an ASCII class that
// is not negated (its characters), a brace of literal branches; null
// for any other node.
function alternatives(node, ci) {
  if (node.tag === N_LITERAL) return [keyOf(node.str, ci)];
  if (node.tag === N_CLASS) {
    if (node.neg) return null;
    const out = [];
    for (const it of node.items) {
      const lo = it.tag === CI_BYTE ? it.b : it.lo;
      const hi = it.tag === CI_BYTE ? it.b : it.hi;
      if (hi >= 0x80 || out.length + Math.max(0, hi - lo) >= MAX_KEY_PRODUCT) return null;
      for (let c = lo; c <= hi; c++) out.push(keyOf(String.fromCharCode(c), ci));
    }
    return out;
  }
  if (node.tag === N_BRACE) {
    const out = [];
    for (const branch of node.branches) {
      const s = seq(branch);
      if (s.length === 1 && s[0].tag === N_LITERAL) {
        out.push(keyOf(s[0].str, ci));
        continue;
      }
      const keys = [];
      if (literalRun(s, false, ci, keys) < s.length) return null;
      for (const k of keys) out.push(k);
    }
    return out;
  }
  return null;
}

// The elements whose segment a chain's shape pins, as [role, node]
// pairs in the order they are tried: the last when a tail ends the
// path, the first when a head starts it, a block's first.
function anchors(chain) {
  const ids = chain.nodes;
  const n = ids.length;
  const g = chain.globstar;
  // Elements before `head` start the path, from `tail` on end it; no
  // globstar: every element is both.
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

// The index of a union's chains, null when they are few. `keys` holds
// the wildcard elements' keys by node (holes for the others); a literal
// element's key is itself.
export function chainIndex(chains, elems, keys, ci) {
  if (chains.length === 0) return null;
  // The literal paths' set, when there is one, is the first chain.
  const set = chains[0].literals !== undefined;
  const from = set ? 1 : 0;
  // A chain is a bit of two words: 64 wildcard chains and the set are
  // one too many, and stay a scan.
  if (chains.length - from <= LINEAR_CHAINS || chains.length > 64) return null;
  const ix = {
    slots: null,
    bits: 0,
    lookups: [0, 0, 0],
    sketchLo: new Int32Array(3 * LOOKUPS),
    sketchHi: new Int32Array(3 * LOOKUPS),
    infix: [],
    restLo: 0,
    restHi: 0,
    // What one probe picks: chains 0–31, 32–63.
    picked: new Int32Array(2),
  };
  const entries = [];
  if (set) {
    for (const path of chains[0].literals) {
      const slash = path.indexOf("/");
      file(ix, R_FIRST, K_EXACT, keyOf(slash < 0 ? path : path.slice(0, slash), ci), 0, entries);
    }
  }
  for (let c = from; c < chains.length; c++) {
    let filed = false;
    for (const [role, node] of anchors(chains[c])) {
      const e = elems[node];
      if (e.kind === EL_LIT && e.litStr.length > 0) {
        file(ix, role, K_EXACT, keyOf(e.litStr, ci), c, entries);
        filed = true;
      } else if (e.kind === EL_WILD && keys !== null && keys[node] !== undefined) {
        const k = keys[node];
        if (k.infix === null) {
          for (const [kind, key] of k.keys) file(ix, role, kind, key, c, entries);
          filed = true;
        } else if (role === R_LAST) {
          // Only a last segment is searched for a literal.
          ix.infix.push([c, k.infix]);
          filed = true;
        }
      }
      if (filed) break;
    }
    if (!filed) {
      if (c < 32) ix.restLo |= 1 << c;
      else ix.restHi |= 1 << (c - 32);
    }
  }
  // Each key and chain once.
  entries.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
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
    slots[at] = ((tagOf(h) << 6) | c) >>> 0;
  }
  ix.slots = slots;
  return ix;
}

// File chain `c` under `key` for `role`.
function file(ix, role, kind, key, c, entries) {
  const short = Math.min(key.len, KEY_BYTES);
  let lookup = 0;
  let len = 0;
  let outer = key.back;
  let compared = short;
  if (kind === K_EXACT) {
    len = key.len;
    compared = key.len;
  } else if (kind === K_PREFIX) {
    lookup = KEY_BYTES + short;
    outer = key.front;
  } else lookup = short;
  ix.lookups[role] |= 1 << lookup;
  const bit = outer & 63;
  const at = role * LOOKUPS + lookup;
  if (bit < 32) ix.sketchLo[at] |= 1 << bit;
  else ix.sketchHi[at] |= 1 << (bit - 32);
  // An exact key's length past 255 runs into the lookup's bits, where it
  // can meet only a suffix or prefix key: a chain more, which its walk
  // turns down.
  entries.push([((lookup << 8) | len) * 2 ** 32 + ((outer & mask(compared)) >>> 0), c]);
}

// A key's hash, from its high bits (the lookup and an exact key's
// length) and its outer characters; the slot is its top bits.
function hashKey(hi, lo) {
  let h = Math.imul(lo ^ Math.imul(hi, 0x9e3779b1), 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  return h ^ (h >>> 16);
}

// The tag a slot keeps of a key: 26 bits of another mix of its hash,
// the top one set, so a slot is never 0.
function tagOf(h) {
  return ((Math.imul(h, 0x2c1b3c6d) >>> 6) & 0x1ffffff) | 0x2000000;
}

// Mark in `ix.picked` the chains `str[s, t)` picks for keys of `role`.
export function probe(ix, role, str, s, t, ci) {
  const n = t - s;
  let todo = ix.lookups[role];
  // A segment's start is packed only for a prefix lookup.
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
    const h = hashKey((lookup << 8) | len, (outer & mask(compared)) >>> 0);
    const tag = tagOf(h);
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

// Mark in `ix.picked` the chains whose literal `str[s, t)` holds.
export function probeInfix(ix, str, s, t, ci) {
  const picked = ix.picked;
  for (const [c, lit] of ix.infix) {
    if (holds(str, s, t, lit, ci)) {
      if (c < 32) picked[0] |= 1 << c;
      else picked[1] |= 1 << (c - 32);
    }
  }
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

// The last KEY_BYTES characters of `str[s, t)`, folded, the last lowest.
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

// The first KEY_BYTES characters of `str[s, t)`, folded, the first
// lowest.
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

// The four bytes of a word, folded.
function foldWord(w) {
  let out = 0;
  for (let i = 0; i < 32; i += 8) out |= fold((w >>> i) & 0xff, true) << i;
  return out >>> 0;
}

// The low `len` bytes of a word.
function mask(len) {
  return len >= KEY_BYTES ? 0xffffffff : (1 << (8 * len)) - 1;
}
