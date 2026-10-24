// The chain index: which chains of a big union a path may match, by
// what its segments are, end with, start with or hold. JS twin of
// `crates/globstar/src/engine/segment/chain_index.rs`: each chain is
// filed under the literal strings one of its anchored segments must
// have — its last (a tail), else its first (a head), else a block's
// first — read off that segment's nodes when the graph is built; the
// literal paths' set under their first segments. The matcher looks up
// a path's own last, first and inner segments and tries only the
// chains they pick (`_indexedChainMatches`); the chain walk decides.

import { N_BRACE, N_CLASS, N_LITERAL, seq } from "../../parse/ast.js";
import { EL_LIT, EL_WILD } from "./elem.js";
import { MAX_CHAINS } from "./chain.js";
import { fold } from "../../bytes.js";

// A union of at most this many chains (the literal set aside) tries
// them in turn after their gates, which costs less than the lookups.
export const LINEAR_CHAINS = 10;

// A segment's strings multiply past this many no more: a product of
// alternatives (`[._]s[a-v][a-z]`) is cut to the nodes that fit; a
// single list (a merged set of names) is kept up to the key room.
const MAX_KEY_PRODUCT = 64;

// The keys a union's segments may give, and the entries its index may
// file: one per unit of the pattern's weight, never fewer than 4096, so
// the index grows with the pattern, not with its chains times their
// keys. A segment whose keys pass what is left has none; an index past
// it is not built.
export function keyRoom(weight) {
  return Math.max(weight, 4096);
}

// A key compares this many outer characters of a segment at most (the
// last of a suffix, the first of a prefix): the walk checks the rest.
// Past 64 chains an exact key compares the whole segment, by its hash.
const KEY_BYTES = 4;

// The base of a key's `whole`, a polynomial in it over the characters.
const BASE = 0x01000193;

// An infix is at most this many first characters of a literal: the
// search for it costs at most that many compares per character of a
// path's last segment, and the walk checks the rest.
const MAX_INFIX = 16;

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

// A string by its length and characters, folded when case-insensitive:
// `back` holds its last KEY_BYTES (the last lowest), `front` its first
// (the first lowest), `whole` all of them (`wholeOf`), and `pow` is
// BASE to its length, to join it to another.
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

const EMPTY_KEY = { len: 0, back: 0, front: 0, whole: 0, pow: 1 };

// `a` then `b`.
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

// What a segment's keys take of the key room.
export function segKeysLen(k) {
  return k.keys === null ? 1 : k.keys.length;
}

// The keys of a segment's nodes (`oneKind`): `{ keys }`, a list of
// [kind, key], or `{ infix }`; null when they are more than `room`. A
// segment that is one brace its branches bind apart — a union's members
// of one segment, `{Makefile,inc.*,*.pc}` — is bound by all their keys,
// each its own kind; a branch without strings leaves it none.
export function segmentKeys(nodes, ci, room) {
  const keys = oneKind(nodes, ci, room);
  if (keys !== null || nodes.length !== 1 || nodes[0].tag !== N_BRACE)
    return keys !== null && segKeysLen(keys) > room ? null : keys;
  const out = [];
  for (const branch of nodes[0].branches) {
    const k = segmentKeys(seq(branch), ci, room - out.length);
    if (k === null || k.keys === null) return null;
    for (const kk of k.keys) out.push(kk);
  }
  return { keys: out, infix: null };
}

// One kind of keys for a segment's nodes: its strings when it is
// literal (`.DS_Store`, `[Dd]ebug`, `{dist,out}`); else those of the
// literal nodes that end it or that start it, whichever bind more
// characters; else the start of its longest literal. Null when it has
// no literal, or a string may be empty.
function oneKind(nodes, ci, room) {
  const suffixes = [];
  const taken = literalRun(nodes, true, ci, room, suffixes);
  if (taken === nodes.length) return strings(K_EXACT, suffixes);
  const prefixes = [];
  literalRun(nodes, false, ci, room, prefixes);
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
    for (let i = 0; i < Math.min(longest.length, MAX_INFIX); i++)
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
// fit MAX_KEY_PRODUCT, appended to `out`; how many nodes they took.
// `back`: from the last node on, each one's strings going in front. A
// node of one string waits in `fixed` for the next choice, so a run
// after a long list passes over the list once.
function literalRun(nodes, back, ci, room, out) {
  // `key`, then the string of the node read after it.
  const join = back ? (key, node) => then(node, key) : then;
  let cur = [EMPTY_KEY];
  let fixed = EMPTY_KEY;
  // The strings so far, then the ones waiting.
  const flush = () => {
    if (fixed.len === 0) return;
    for (let i = 0; i < cur.length; i++) cur[i] = join(cur[i], fixed);
    fixed = EMPTY_KEY;
  };
  let taken = 0;
  for (let k = 0; k < nodes.length; k++) {
    const alts = alternatives(nodes[back ? nodes.length - 1 - k : k], ci, room);
    if (alts === null) break;
    if (alts.length === 1) fixed = join(fixed, alts[0]);
    else {
      if (cur.length > 1 && cur.length * alts.length > MAX_KEY_PRODUCT) break;
      flush();
      const next = [];
      for (const o of cur) for (const a of alts) next.push(join(o, a));
      cur = next;
    }
    taken++;
  }
  flush();
  for (const k of cur) out.push(k);
  return taken;
}

// The strings one literal node matches: a literal, an ASCII class that
// is not negated (its characters), a brace of literal branches; null
// for any other node, and for a brace of more than `room` strings.
function alternatives(node, ci, room) {
  if (node.tag === N_LITERAL) return [keyOf(node.str, ci)];
  if (node.tag === N_CLASS) {
    if (node.neg) return null;
    const out = [];
    for (const { lo, hi } of node.ranges) {
      if (hi >= 0x80 || out.length + Math.max(0, hi - lo) >= MAX_KEY_PRODUCT) return null;
      for (let c = lo; c <= hi; c++) out.push(keyOf(String.fromCharCode(c), ci));
    }
    return out;
  }
  if (node.tag === N_BRACE) {
    const out = [];
    for (const branch of node.branches) {
      const s = seq(branch);
      if (s.length === 1 && s[0].tag === N_LITERAL) out.push(keyOf(s[0].str, ci));
      else if (literalRun(s, false, ci, room, out) < s.length) return null;
      if (out.length > room) return null;
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

// The index of a union's chains over `graph`, null when they are few or
// their entries pass the key room, and past 64 when a call may walk
// more than MAX_CHAINS of them by one key or without any. The graph's
// keys are the wildcard elements', by node (holes for the others); a
// literal element's key is itself.
export function chainIndex(chains, graph, ci) {
  if (chains.length === 0) return null;
  const { elems, keys } = graph;
  // The literal paths' set, when there is one, is the first chain.
  const set = chains[0].literals !== undefined;
  const from = set ? 1 : 0;
  if (chains.length - from <= LINEAR_CHAINS) return null;
  // The low bits of a slot that name its chain: 6 for a union of at
  // most 64, whose picks are bits of two words, more for a bigger one.
  const idBits = Math.max(6, 32 - Math.clz32(chains.length - 1));
  const many = idBits > 6;
  const ix = {
    slots: null,
    bits: 0,
    idBits,
    lookups: [0, 0, 0],
    sketchLo: new Int32Array(3 * LOOKUPS),
    sketchHi: new Int32Array(3 * LOOKUPS),
    infix: [],
    // The chains no key files: chains 0–31, 32–63, or past 64 a list.
    restLo: 0,
    restHi: 0,
    rest: [],
    // What one probe picks: chains 0–31, 32–63.
    picked: new Int32Array(2),
    // Past 64, the chains a call picks, in the order picked: how many,
    // and how many of them it has walked; and the call that last listed
    // each chain (`epoch`), so a chain is listed once a call.
    list: many ? new Int32Array(chains.length) : null,
    listed: 0,
    tried: 0,
    stamp: many ? new Int32Array(chains.length) : null,
    epoch: 0,
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
      if (many) ix.rest.push(c);
      else if (c < 32) ix.restLo |= 1 << c;
      else ix.restHi |= 1 << (c - 32);
    }
    if (entries.length > keyRoom(graph.weight)) return null;
  }
  // Each key and chain once.
  entries.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  // Past a word's bits a call walks what one key picks, and those no
  // key files or a literal search picks, each up to what a scan would
  // try.
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

// File chain `c` under `key` for `role`.
function file(ix, role, kind, key, c, entries) {
  const short = Math.min(key.len, KEY_BYTES);
  // The lookup, the length it holds, the outer characters the sketch
  // reads, what it compares.
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

// A key's high bits: its role, its lookup, the length an exact key holds
// (past 255 as 255).
const lookupHi = (role, lookup, len) => (role << 16) | (lookup << 8) | Math.min(len, 255);

// A key's hash, from its high bits (`lookupHi`) and what it compares;
// the slot is its top bits.
function hashKey(hi, lo) {
  let h = Math.imul(lo ^ Math.imul(hi, 0x9e3779b1), 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  return h ^ (h >>> 16);
}

// The tag a slot keeps of a key: the bits of another mix of its hash
// above the chain's `idBits`, the top one set, so a slot is never 0.
function tagOf(h, idBits) {
  const top = 1 << (31 - idBits);
  return ((Math.imul(h, 0x2c1b3c6d) >>> idBits) & (top - 1)) | top;
}

// Mark chain `c` picked: a bit of `ix.picked`, or past 64 on the call's
// list, once a call.
function mark(ix, c) {
  if (ix.stamp === null) {
    if (c < 32) ix.picked[0] |= 1 << c;
    else ix.picked[1] |= 1 << (c - 32);
  } else if (ix.stamp[c] !== ix.epoch) {
    ix.stamp[c] = ix.epoch;
    ix.list[ix.listed++] = c;
  }
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

// `probe` past 64 chains: each chain the call's list takes once
// (`ix.stamp`), and an exact key compares the whole segment, as names
// that differ only inside (`n17.js`, `n27.js`) would crowd one key. A
// function of its own, so that `probe` stays as lean as one word of
// bits allows.
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

// Mark the chains whose literal `str[s, t)` holds.
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

// The characters of `str[s, t)`, folded, as a polynomial in BASE (the
// last its constant term): what an exact key compares.
function wholeOf(str, s, t, ci) {
  let h = 0;
  for (let i = s; i < t; i++) h = (Math.imul(h, BASE) + fold(str.charCodeAt(i) & 0xff, ci)) | 0;
  return h >>> 0;
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
