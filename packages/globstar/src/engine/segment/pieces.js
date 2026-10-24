// Brace-free in-segment matcher — the `WK_PIECES` wild backend. JS twin
// of `crates/globstar/src/engine/segment/pieces.rs`: a segment of
// literals, `?`s and classes between `*`s (`a*b?[c-e]*f`) is the pieces
// its `*`s cut, each of a fixed length. The first is matched at the
// segment's start, the last at its end, and each one between at its
// leftmost place after the one before: that leaves the most room to the
// rest, so no match is missed and no place is tried twice. A piece up to
// MAX_SCAN positions is tried at each place in turn; a longer one is
// found by shift-and, a word operation per 32 positions a unit, so a
// segment costs O(n·m/32) at worst.

import { N_ANYCHAR, N_CLASS, N_LITERAL, N_STAR } from "../../parse/ast.js";
import { NEVER_LEN, exact } from "./elem.js";
import { byteSet, setEmpty, setHas } from "./seg-nfa.js";

// A piece up to this many positions is tried at each place in turn, at
// most this many unit tests a place; a longer one is searched by
// shift-and.
const MAX_SCAN = 32;

// What a position takes, one code each: a byte (0-255, ASCII-lowercased
// when case-insensitive), any byte (ANY, `?`), or class `k` of the
// pieces' classes (CLASS + k).
const ANY = 256;
const CLASS = 257;

// The segment's pieces and what the graph reads off them, as `makeWild`
// fields, or null when it holds a brace (an NFA's). What the graph reads
// is what the NFA's would be: NEVER_LEN when a class takes no byte, and
// protected unless it starts with a literal `.` or a positive class
// listing it; its wildcards alone take no segment, as a literal or a
// class stands in each (only `?`s and `*`s are an affix).
export function compilePieces(nodes, dot, ci) {
  for (const n of nodes) {
    if (n.tag !== N_LITERAL && n.tag !== N_ANYCHAR && n.tag !== N_STAR && n.tag !== N_CLASS) {
      return null;
    }
  }
  const at = [];
  const ends = [];
  const classes = [];
  const index = new Map();
  // Only `?`s and negated classes see a difference between a path
  // string's UTF-16 units and its UTF-8 bytes, as in the NFA.
  let countsBytes = false;
  let star = false;
  for (const node of nodes) {
    const tag = node.tag;
    if (tag === N_LITERAL) {
      const str = node.str;
      for (let i = 0; i < str.length; i++) at.push(fold(str.charCodeAt(i), ci));
    } else if (tag === N_ANYCHAR) {
      countsBytes = true;
      at.push(ANY);
    } else if (tag === N_CLASS) {
      if (node.neg) countsBytes = true;
      const set = byteSet(node, ci);
      const key = set.bits.join();
      let k = index.get(key);
      if (k === undefined) {
        if (classes.length > 0xffff) return null;
        k = classes.length;
        index.set(key, k);
        classes.push(set);
      }
      at.push(CLASS + k);
    } else if (!star) {
      // A run of `*`s is one: no empty piece between.
      ends.push(at.length);
    }
    star = tag === N_STAR;
  }
  ends.push(at.length);
  const first = nodes[0];
  const canLitDot =
    first.tag === N_LITERAL
      ? first.str.charCodeAt(0) === 0x2e
      : first.tag === N_CLASS && !first.neg && setHas(classes[at[0] - CLASS], 0x2e);
  return {
    matcher: new Pieces(at, ends, classes, countsBytes, ci),
    minLen: classes.some(setEmpty) ? NEVER_LEN : at.length,
    variable: ends.length > 1,
    dotProtect: !dot && !canLitDot,
    anySeg: false,
  };
}

const fold = (c, ci) => (ci && c >= 0x41 && c <= 0x5a ? c | 0x20 : c);

// Does a position take unit `c`? A unit past Latin-1 is no class's.
function takes(code, c, classes, ci) {
  if (code < ANY) return fold(c, ci) === code;
  return code === ANY || (c <= 0xff && setHas(classes[code - CLASS], c));
}

class Pieces {
  constructor(at, ends, classes, countsBytes, ci) {
    // Every piece's positions, piece after piece; piece `k` ends at
    // `ends[k]`, and starts where the one before ends (the first at 0).
    this.at = exact(at);
    this.ends = exact(ends);
    this.classes = exact(classes);
    this.countsBytes = countsBytes;
    // The shift-and tables of the pieces between the first and the last
    // longer than MAX_SCAN, in order.
    const long = [];
    for (let k = 1; k + 1 < ends.length; k++) {
      if (ends[k] - ends[k - 1] > MAX_SCAN) {
        long.push(new ShiftAnd(at, ends[k - 1], ends[k], classes, ci));
      }
    }
    this.long = long;
  }

  // Does `str[s, t)` match, its length checked against the pieces' (at
  // least theirs, or theirs alone without a `*`)? The last piece first:
  // where names differ most.
  matches(str, s, t, ci) {
    const ends = this.ends;
    const last = ends.length - 1;
    if (last === 0) return this.fits(0, ends[0], str, s, ci);
    const lastFrom = ends[last - 1];
    const to = t - (ends[last] - lastFrom);
    if (!this.fits(lastFrom, ends[last], str, to, ci) || !this.fits(0, ends[0], str, s, ci)) {
      return false;
    }
    let from = s + ends[0];
    let long = 0;
    for (let k = 1; k < last; k++) {
      const lo = ends[k - 1];
      const hi = ends[k];
      const end =
        hi - lo > MAX_SCAN
          ? this.long[long++].find(str, from, to)
          : this.scan(lo, hi, str, from, to, ci);
      if (end < 0) return false;
      from = end;
    }
    return true;
  }

  // Do the positions `at[lo, hi)` take the units from `p` on, the last
  // first?
  fits(lo, hi, str, p, ci) {
    const at = this.at;
    for (let i = hi - 1; i >= lo; i--) {
      if (!takes(at[i], str.charCodeAt(p + i - lo), this.classes, ci)) return false;
    }
    return true;
  }

  // Where the leftmost place of the piece `at[lo, hi)` within
  // `str[from, to)` ends (-1: none), trying each in turn.
  scan(lo, hi, str, from, to, ci) {
    const m = hi - lo;
    for (let k = from; k + m <= to; k++) if (this.fits(lo, hi, str, k, ci)) return k + m;
    return -1;
  }
}

// A long piece's shift-and tables: bit `i` of a byte's mask is set when
// the piece's position `i` takes the byte. Bytes every position takes or
// refuses alike share a mask, so there are at most 256 and mostly a few.
class ShiftAnd {
  constructor(at, lo, hi, classes, ci) {
    // The bytes in parts, split by each position that takes some of a
    // part: a byte or a class once, and `?` takes them all.
    const part = new Uint8Array(256);
    let parts = 1;
    const seen = new Set();
    const split = new Int16Array(512);
    for (let i = lo; i < hi; i++) {
      const code = at[i];
      if (code === ANY || seen.has(code) || parts === 256) continue;
      seen.add(code);
      split.fill(-1);
      let count = 0;
      for (let b = 0; b < 256; b++) {
        const key = part[b] * 2 + (takes(code, b, classes, ci) ? 1 : 0);
        if (split[key] === -1) split[key] = count++;
        part[b] = split[key];
      }
      parts = count;
    }
    // A byte that stands for each part.
    const stands = new Uint8Array(256);
    for (let b = 0; b < 256; b++) stands[part[b]] = b;
    const m = hi - lo;
    const words = (m + 31) >>> 5;
    const masks = new Int32Array(parts * words);
    for (let i = 0; i < m; i++) {
      const code = at[lo + i];
      const w = i >>> 5;
      const bit = 1 << (i & 31);
      // Case-insensitively both cases are in one part: every position
      // takes both or neither.
      if (code < ANY) masks[part[code] * words + w] |= bit;
      else {
        for (let q = 0; q < parts; q++) {
          if (takes(code, stands[q], classes, ci)) masks[q * words + w] |= bit;
        }
      }
    }
    // Each byte's mask, by index into `masks`, `words` words each.
    this.maskOf = part;
    this.masks = masks;
    this.words = words;
    // The piece's last position, as a bit of the last word.
    this.top = 1 << ((m - 1) & 31);
  }

  // Where the leftmost place of the piece within `str[from, to)` ends
  // (-1: none).
  find(str, from, to) {
    const words = this.words;
    const masks = this.masks;
    // Bit `i`: the piece's first `i + 1` positions take the units up to
    // here.
    const d = new Int32Array(words);
    for (let j = from; j < to; j++) {
      const c = str.charCodeAt(j);
      // A unit past Latin-1 is in a string matched as it is, whose
      // pieces take no byte past 0x7f: as 0xff, it is taken by none.
      const row = this.maskOf[c <= 0xff ? c : 0xff] * words;
      let carry = 1;
      for (let w = 0; w < words; w++) {
        const x = d[w];
        d[w] = ((x << 1) | carry) & masks[row + w];
        carry = x >>> 31;
      }
      if ((d[words - 1] & this.top) !== 0) return j + 1;
    }
    return -1;
  }
}
