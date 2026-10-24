// In-segment wildcard matchers: what a segment with a wildcard compiles
// to (`compileWild`) and how it takes a path segment (`wildConsumes`):
// an affix, a suffix set or pieces here, the in-segment NFA in
// `seg-nfa.js`. JS twin of `crates/globstar/src/engine/segment/wild.rs`.

import { N_ANYCHAR, N_BRACE, N_LITERAL, N_STAR, seq, N_CLASS } from "../../parse/ast.js";
import { affixEq } from "../../bytes.js";
import { flat, isAscii, utf8Latin1, utf8Length } from "../../utf8.js";
import { exact } from "./graph.js";
import { compileSegNfa } from "./seg-nfa.js";
import { byteSet, setEmpty, setHas } from "./bits.js";

// In-segment wildcard kinds.
export const WK_AFFIX = 0;
export const WK_SUFFIXES = 1;
export const WK_GENERIC = 2;
export const WK_PIECES = 3;

// The `minLen` of a segment no path segment can fill: one with an
// escaped `\` on Windows, where a segment never holds a separator, or an
// in-segment NFA that accepts nothing.
export const NEVER_LEN = 0x7fffffff;

// ── compiling ───────────────────────────────────────────────────────

// The bytes of the literal nodes `nodes[..to]`.
export function concatLits(nodes, to) {
  let s = "";
  for (let i = 0; i < to; i++) s += nodes[i].str;
  return s;
}

export function makeWild(kind, fields) {
  return {
    kind,
    prefixStr: flat(fields.prefixStr ?? ""),
    suffixStr: flat(fields.suffixStr ?? ""),
    // A set's non-empty suffixes grouped by last character (`lastGroup`,
    // one key per suffix in `suffixKeys`, ascending), each group in
    // pattern order. The Rust twin scans a few in turn and indexes more
    // by a table of group starts; a typed array costs V8 ~200 ns to
    // allocate, so here a group is found by a binary search of the keys.
    suffixSetStr: fields.suffixSetStr?.map(flat) ?? null,
    suffixKeys: fields.suffixKeys ?? null,
    // One suffix is empty: it ends anything.
    suffixEmpty: fields.suffixEmpty ?? false,
    minLen: fields.minLen ?? 0,
    variable: fields.variable ?? true,
    dotProtect: fields.dotProtect ?? false,
    anychars: fields.anychars ?? 0,
    // The pieces (WK_PIECES) or the in-segment NFA (WK_GENERIC).
    matcher: fields.matcher ?? null,
    // Its wildcards alone match every non-empty segment (`*`, `?*`,
    // `{a,*}`); drives `matchDir`'s all-below bit.
    anySeg: fields.anySeg ?? false,
  };
}

export function compileWild(nodes, dot, ci) {
  let idx = 0;
  while (idx < nodes.length && nodes[idx].tag === N_LITERAL) idx++;
  const prefix = concatLits(nodes, idx);
  let anychars = 0;
  let hasStar = false;
  while (idx < nodes.length) {
    const t = nodes[idx].tag;
    if (t === N_STAR) hasStar = true;
    else if (t === N_ANYCHAR) anychars += 1;
    else break;
    idx += 1;
  }
  // A literal product after the wildcards (one empty suffix when nothing
  // follows them) keeps the segment anchored at both ends; anything else
  // is pieces, or with a brace runs the NFA.
  const suffixes = suffixProduct(nodes, idx);
  if (suffixes === null) {
    const pieces = compilePieces(nodes, dot, ci);
    if (pieces !== null) return makeWild(WK_PIECES, pieces);
    return makeWild(WK_GENERIC, compileSegNfa(nodes, dot, ci));
  }
  const fields = {
    prefixStr: prefix,
    minLen: prefix.length + anychars,
    variable: hasStar,
    dotProtect: !dot && prefix.length === 0 && (hasStar || anychars > 0),
    anychars,
    // `*` or `?*` with nothing literal around it, up to a suffix that
    // may be empty.
    anySeg: prefix.length === 0 && hasStar && anychars <= 1 && suffixes.some((s) => s.length === 0),
  };
  if (suffixes.length === 1) {
    fields.suffixStr = suffixes[0];
    fields.minLen += suffixes[0].length;
    return makeWild(WK_AFFIX, fields);
  }
  // A counting sort by group keeps the pattern's order within one.
  const starts = GROUP_SCRATCH.fill(0);
  let empty = false;
  for (const s of suffixes) {
    if (s.length === 0) empty = true;
    else starts[lastGroup(s.charCodeAt(s.length - 1), ci) + 1]++;
  }
  for (let g = 1; g < 65; g++) starts[g] += starts[g - 1];
  const set = [];
  const keys = [];
  for (const s of suffixes) {
    if (s.length === 0) continue;
    const g = lastGroup(s.charCodeAt(s.length - 1), ci);
    const k = starts[g]++;
    set[k] = s;
    keys[k] = g;
  }
  fields.suffixEmpty = empty;
  fields.suffixSetStr = set;
  fields.suffixKeys = exact(keys);
  return makeWild(WK_SUFFIXES, fields);
}

// What `w` takes through its wildcards alone (dot=true), by segment
// length, as `lenBits` and `lenFrom` hold for an NFA (see `wildLens`
// in seg-nfa.js): bit L for every segment of L units (1 ≤ L ≤ 31), and
// every length from `wildLenFrom` on (0: none). Only `?`s and `*`s with
// nothing literal around them (a suffix that may be empty) take every
// segment of a length.
export function wildLenBits(w) {
  if (w.kind === WK_GENERIC) return w.matcher.lenBits;
  if (w.variable || !bare(w) || w.minLen < 1 || w.minLen > 31) return 0;
  return 1 << w.minLen;
}

export function wildLenFrom(w) {
  if (w.kind === WK_GENERIC) return w.matcher.lenFrom;
  if (!w.variable || !bare(w)) return 0;
  const from = Math.max(w.minLen, 1);
  return from <= 32 ? from : 0;
}

const bare = (w) =>
  w.prefixStr.length === 0 && (w.kind === WK_AFFIX ? w.suffixStr.length === 0 : w.suffixEmpty);

// The group of the suffixes ending in character `c`.
function lastGroup(c, ci) {
  if (ci && c >= 0x41 && c <= 0x5a) c += 32;
  return c & 63;
}

// Where each group starts while a set is built; one for all builds.
const GROUP_SCRATCH = new Int32Array(65);

// Literal suffix alternatives one segment enumerates (`*.{a,b,…}`, a
// factored union of names). A product of alternatives (`{a,b}{c,d}…`)
// past this many suffixes stays an NFA, which holds it without
// multiplying it out; a single list is no product and is enumerated
// whatever its length.
const MAX_SUFFIX_PRODUCT = 1024;
// The enumerated suffixes, one byte more each, may not take more than
// this or four times their nodes: a long literal next to a product
// would be copied into every suffix.
const MAX_SUFFIX_BYTES = 64 * 1024;

function suffixProduct(nodes, from) {
  if (from + 1 === nodes.length && nodes[from].tag === N_LITERAL) {
    return [nodes[from].str];
  }
  let parts = [""];
  // Σ (part + 1) bytes, and what the nodes themselves hold.
  let size = 1;
  let source = 1;
  const fits = () => size <= Math.max(MAX_SUFFIX_BYTES, source * 4);
  for (let i = from; i < nodes.length; i++) {
    const node = nodes[i];
    if (node.tag === N_LITERAL) {
      size += parts.length * node.str.length;
      source += node.str.length;
      if (!fits()) return null;
      parts = parts.map((p) => p + node.str);
    } else if (node.tag === N_BRACE) {
      const lits = [];
      let branchBytes = 0;
      for (const branch of node.branches) {
        const b = seq(branch);
        if (b.length === 0) lits.push("");
        else if (b.length === 1 && b[0].tag === N_LITERAL) lits.push(b[0].str);
        else return null;
        branchBytes += lits[lits.length - 1].length + 1;
      }
      if (parts.length > 1 && parts.length * lits.length > MAX_SUFFIX_PRODUCT) return null;
      // Σ (|p| + |l| + 1) over every pair.
      size = lits.length * (size - parts.length) + parts.length * branchBytes;
      source += branchBytes;
      if (!fits()) return null;
      const next = [];
      for (const p of parts) for (const l of lits) next.push(p + l);
      parts = next;
    } else {
      return null;
    }
  }
  return parts;
}

// ── matching ────────────────────────────────────────────────────────

// On a path string (not `m.byteOnly`: the pattern is ASCII) the
// literal parts compare as they are; `?` counts UTF-8 bytes, so the
// length rules read the segment's UTF-8 length, and pieces or an
// in-segment NFA that count run on the segment's bytes.
export function wildConsumes(m, w, str, s, t) {
  if (w.dotProtect && t > s && str.charCodeAt(s) === 0x2e) return false;
  const ci = m.ci;
  // The in-segment NFA has none: it counts on its own.
  const len = w.anychars > 0 && !m.byteOnly ? utf8Length(str, s, t) : t - s;
  switch (w.kind) {
    case WK_AFFIX: {
      const need = w.minLen;
      if (len < need || (!w.variable && len !== need)) return false;
      if (w.prefixStr.length > 0 && !affixEq(w.prefixStr, str, s, ci)) return false;
      if (w.suffixStr.length > 0 && !affixEq(w.suffixStr, str, t - w.suffixStr.length, ci)) {
        return false;
      }
      return true;
    }
    case WK_SUFFIXES: {
      const p = w.prefixStr;
      // `minLen` holds the prefix and the `?`s, so `room` is what is
      // left for a suffix.
      const room = len - w.minLen;
      if (room < 0 || (p.length > 0 && !affixEq(p, str, s, ci))) return false;
      if (w.suffixEmpty && (w.variable || room === 0)) return true;
      if (room === 0) return false;
      const g = lastGroup(str.charCodeAt(t - 1), ci);
      const keys = w.suffixKeys;
      // The group's first suffix, by binary search of the keys.
      let lo = 0;
      let hi = keys.length;
      while (lo < hi) {
        const mid = (lo + hi) >>> 1;
        if (keys[mid] < g) lo = mid + 1;
        else hi = mid;
      }
      const set = w.suffixSetStr;
      for (let k = lo; k < keys.length && keys[k] === g; k++) {
        const suf = set[k];
        if (suf.length > room || (!w.variable && suf.length !== room)) continue;
        if (affixEq(suf, str, t - suf.length, ci)) return true;
      }
      return false;
    }
    case WK_PIECES: {
      let at = str;
      let from = s;
      let to = t;
      if (w.matcher.countsBytes && !m.byteOnly && !isAscii(str, s, t)) {
        at = utf8Latin1(str.slice(s, t));
        from = 0;
        to = at.length;
      }
      const n = to - from;
      if (n < w.minLen || (!w.variable && n !== w.minLen)) return false;
      return w.matcher.matches(at, from, to, ci);
    }
    default: {
      const nfa = w.matcher;
      if (nfa.countsBytes && !m.byteOnly && !isAscii(str, s, t)) {
        const b = utf8Latin1(str.slice(s, t));
        return nfa.matches(b, 0, b.length);
      }
      return nfa.matches(str, s, t);
    }
  }
}

// ── pieces ──────────────────────────────────────────────────────────

// Brace-free in-segment matcher — the `WK_PIECES` wild backend: a
// segment of literals, `?`s and classes between `*`s (`a*b?[c-e]*f`) is the pieces
// its `*`s cut, each of a fixed length. The first is matched at the
// segment's start, the last at its end, and each one between at its
// leftmost place after the one before: that leaves the most room to the
// rest, so no match is missed and no place is tried twice. A piece up to
// MAX_SCAN positions is tried at each place in turn; a longer one is
// found by shift-and, a word operation per 32 positions a unit, so a
// segment costs O(n·m/32) at worst.

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
function compilePieces(nodes, dot, ci) {
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

// `bytes.js`'s `fold`, copied: the per-unit compare below would pay a
// module-cell load for an imported one.
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
