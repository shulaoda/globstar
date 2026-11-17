import { N_ANYCHAR, N_BRACE, N_LITERAL, N_STAR, seq, N_CLASS } from "../../parse/ast.js";
import { affixEq } from "../bytes.js";
import { isAscii, utf8Latin1, utf8Length } from "../../utf8.js";
import { exact } from "./graph.js";
import { byteSet, setEmpty, setHas } from "./bits.js";
import { compileSegNfa } from "./seg-nfa.js";

export const WK_AFFIX = 0;
export const WK_SUFFIXES = 1;
export const WK_GENERIC = 2;
export const WK_PIECES = 3;

export const NEVER_LEN = 0x7fffffff;

const MAX_SUFFIX_PRODUCT = 1024;

const GROUP_SCRATCH = new Int32Array(65);

export const ANY = 256;
export const CLASS = 257;

export function concatLits(nodes, to) {
  let s = "";
  for (let i = 0; i < to; i++) s += nodes[i].str;
  return s;
}

export function makeWild(kind, fields) {
  return {
    kind,
    prefixStr: fields.prefixStr ?? "",
    suffixStr: fields.suffixStr ?? "",
    suffixSetStr: fields.suffixSetStr ?? null,
    suffixKeys: fields.suffixKeys ?? null,
    suffixEmpty: fields.suffixEmpty ?? false,
    minLen: fields.minLen ?? 0,
    variable: fields.variable ?? true,
    dotProtect: fields.dotProtect ?? false,
    anychars: fields.anychars ?? 0,
    matcher: fields.matcher ?? null,
    anySeg: fields.anySeg ?? false,
  };
}

export function compileWild(nodes, dot, ci, pools) {
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
  const suffixes = suffixProduct(nodes, idx, pools);
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
    anySeg: prefix.length === 0 && hasStar && anychars <= 1 && suffixes.some((s) => s.length === 0),
  };
  if (suffixes.length === 1) {
    fields.suffixStr = suffixes[0];
    fields.minLen += suffixes[0].length;
    return makeWild(WK_AFFIX, fields);
  }
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

function lastGroup(c, ci) {
  if (ci && c >= 0x41 && c <= 0x5a) c += 32;
  return c & 63;
}

function suffixProduct(nodes, from, pools) {
  if (from + 1 === nodes.length && nodes[from].tag === N_LITERAL) {
    return [nodes[from].str];
  }
  const steps = [];
  let count = 1;
  let size = 1;
  let source = 1;
  for (let i = from; i < nodes.length; i++) {
    const node = nodes[i];
    if (node.tag === N_LITERAL) {
      size += count * node.str.length;
      source += node.str.length;
      steps.push([node.str]);
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
      if (count > 1 && count * lits.length > MAX_SUFFIX_PRODUCT) return null;
      size = lits.length * (size - count) + count * branchBytes;
      source += branchBytes;
      count *= lits.length;
      steps.push(lits);
    } else {
      return null;
    }
    if (size > source * 4 + pools.suffixPool) return null;
  }
  pools.suffixPool -= Math.max(0, size - source * 4);
  let parts = [""];
  for (const lits of steps) {
    const next = [];
    for (const p of parts) for (const l of lits) next.push(p + l);
    parts = next;
  }
  return parts;
}

export function wildWeight(w) {
  if (w.kind === WK_AFFIX) return 1;
  if (w.kind === WK_SUFFIXES) return w.suffixSetStr.length + 1;
  if (w.kind === WK_PIECES) return Math.max(w.matcher.at.length, 1);
  return w.matcher.kinds.length;
}

export function wildConsumes(m, w, str, s, t) {
  if (w.dotProtect && t > s && str.charCodeAt(s) === 0x2e) return false;
  const ci = m.ci;
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
      const room = len - w.minLen;
      if (room < 0 || (p.length > 0 && !affixEq(p, str, s, ci))) return false;
      if (w.suffixEmpty && (w.variable || room === 0)) return true;
      if (room === 0) return false;
      const g = lastGroup(str.charCodeAt(t - 1), ci);
      const keys = w.suffixKeys;
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
    matcher: new Pieces(at, ends, classes, countsBytes),
    minLen: classes.some(setEmpty) ? NEVER_LEN : at.length,
    variable: ends.length > 1,
    dotProtect: !dot && !canLitDot,
    anySeg: false,
  };
}

const fold = (c, ci) => (ci && c >= 0x41 && c <= 0x5a ? c | 0x20 : c);

function takes(code, c, classes, ci) {
  if (code < ANY) return fold(c, ci) === code;
  return code === ANY || (c <= 0xff && setHas(classes[code - CLASS], c));
}

class Pieces {
  constructor(at, ends, classes, countsBytes) {
    this.at = exact(at);
    this.ends = exact(ends);
    this.classes = exact(classes);
    this.countsBytes = countsBytes;
  }

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
    for (let k = 1; k < last; k++) {
      const end = this.scan(ends[k - 1], ends[k], str, from, to, ci);
      if (end < 0) return false;
      from = end;
    }
    return true;
  }

  fits(lo, hi, str, p, ci) {
    const at = this.at;
    for (let i = hi - 1; i >= lo; i--) {
      if (!takes(at[i], str.charCodeAt(p + i - lo), this.classes, ci)) return false;
    }
    return true;
  }

  scan(lo, hi, str, from, to, ci) {
    const m = hi - lo;
    for (let k = from; k + m <= to; k++) if (this.fits(lo, hi, str, k, ci)) return k + m;
    return -1;
  }
}
