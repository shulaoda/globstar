// In-segment wildcard matchers: what a segment with a wildcard compiles
// to (`compileWild`) and how it takes a path segment (`wildConsumes`).
// JS twin of `crates/globstar/src/engine/segment/wild.rs`.

import { N_ANYCHAR, N_BRACE, N_LITERAL, N_STAR, seq } from "../../parse/ast.js";
import { asciiLower, eqByteCi } from "../../bytes.js";
import { flat, isAscii, utf8Latin1, utf8Length } from "../../utf8.js";
import { WK_AFFIX, WK_AFFIX_SET, WK_AFFIX_INDEX, WK_GENERIC, exact } from "./elem.js";
import { compileSegNfa } from "./seg-nfa.js";

const EMPTY = "";

// The bytes of the literal nodes `nodes[from..to]`.
export function concatLits(nodes, from, to) {
  let s = "";
  for (let i = from; i < to; i++) s += nodes[i].str;
  return s;
}

// ── compiling ───────────────────────────────────────────────────────

export function makeWild(kind, fields) {
  return {
    kind,
    prefixStr: flat(fields.prefixStr ?? ""),
    suffixStr: flat(fields.suffixStr ?? ""),
    suffixSetStr: fields.suffixSetStr?.map(flat) ?? null,
    runs: fields.runs ?? null,
    minLen: fields.minLen ?? 0,
    variable: fields.variable ?? true,
    dotProtect: fields.dotProtect ?? false,
    anychars: fields.anychars ?? 0,
    nfa: fields.nfa ?? null,
    // Its wildcards alone match every non-empty segment (`*`, `?*`,
    // `{a,*}`); drives `matchDir`'s all-below bit.
    anySeg: fields.anySeg ?? false,
  };
}

export function compileWild(nodes, dot, ci) {
  let idx = 0;
  while (idx < nodes.length && nodes[idx].tag === N_LITERAL) idx++;
  const prefix = concatLits(nodes, 0, idx);
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
  // runs the NFA.
  const suffixes = suffixProduct(nodes, idx);
  if (suffixes === null) return makeWild(WK_GENERIC, compileSegNfa(nodes, dot, ci));
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
  if (suffixes.length <= LINEAR_SUFFIXES) {
    fields.suffixSetStr = suffixes;
    return makeWild(WK_AFFIX_SET, fields);
  }
  const { set, runs } = indexSuffixes(suffixes, ci);
  fields.suffixSetStr = set;
  fields.runs = runs;
  return makeWild(WK_AFFIX_INDEX, fields);
}

// Literal suffix alternatives one segment enumerates (`*.{a,b,…}`, a
// factored union of names). A product of alternatives (`{a,b}{c,d}…`)
// past this many suffixes stays an NFA, which holds it without
// multiplying it out; a single list is no product and is enumerated
// whatever its length.
const MAX_SUFFIX_PRODUCT = 1024;
// An affix set up to this size is scanned in pattern order; a larger
// one is sorted and binary-searched per suffix length.
const LINEAR_SUFFIXES = 8;
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
        if (b.length === 0) lits.push(EMPTY);
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

// Past LINEAR_SUFFIXES the set is sorted by (length, bytes), ASCII-
// lowercased when case-insensitive, and indexed per length so
// wildConsumes can binary-search it. Latin-1 strings order by byte.
function indexSuffixes(suffixes, ci) {
  let set = suffixes;
  if (ci) set = set.map(asciiLower);
  set.sort((a, b) => a.length - b.length || (a < b ? -1 : a > b ? 1 : 0));
  set = set.filter((s, i) => i === 0 || s !== set[i - 1]);
  const runs = [];
  for (let i = 0; i < set.length; i++) {
    const len = set[i].length;
    const last = runs.length > 0 ? runs[runs.length - 1] : null;
    if (last !== null && last.len === len) last.end = i + 1;
    else runs.push({ len, start: i, end: i + 1 });
  }
  return { set: exact(set), runs: exact(runs) };
}

// ── matching ────────────────────────────────────────────────────────

export function litEq(lit, str, s, t, ci) {
  if (t - s !== lit.length) return false;
  if (!ci) return str.startsWith(lit, s);
  for (let i = 0; i < lit.length; i++) {
    if (!eqByteCi(lit.charCodeAt(i), str.charCodeAt(s + i))) return false;
  }
  return true;
}

export function affixEq(part, str, at, ci) {
  if (!ci) return str.startsWith(part, at);
  for (let i = 0; i < part.length; i++) {
    if (!eqByteCi(part.charCodeAt(i), str.charCodeAt(at + i))) return false;
  }
  return true;
}

// Binary search of `set[lo, hi)` (sorted, one length) for the segment
// tail `str[at, at + len)`, folding the tail's ASCII case when `ci`.
function runHas(set, lo, hi, str, at, len, ci) {
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const c = cmpTail(set[mid], str, at, len, ci);
    if (c === 0) return true;
    if (c < 0) lo = mid + 1;
    else hi = mid;
  }
  return false;
}

function cmpTail(suf, str, at, len, ci) {
  for (let i = 0; i < len; i++) {
    const a = suf.charCodeAt(i);
    let b = str.charCodeAt(at + i);
    if (ci && b >= 0x41 && b <= 0x5a) b += 32;
    if (a !== b) return a < b ? -1 : 1;
  }
  return 0;
}

// On a path string (not `m.byteOnly`: the pattern is ASCII) the
// literal parts compare as they are; `?` counts UTF-8 bytes, so the
// length rules read the segment's UTF-8 length, and an in-segment NFA
// that counts runs on the segment's bytes.
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
    case WK_AFFIX_SET: {
      const p = w.prefixStr;
      if (len < p.length || (p.length > 0 && !affixEq(p, str, s, ci))) return false;
      const set = w.suffixSetStr;
      for (let i = 0; i < set.length; i++) {
        const suf = set[i];
        const need = w.minLen + suf.length;
        if (len < need || (!w.variable && len !== need)) continue;
        if (suf.length === 0 || affixEq(suf, str, t - suf.length, ci)) return true;
      }
      return false;
    }
    case WK_AFFIX_INDEX: {
      const p = w.prefixStr;
      if (len < p.length || (p.length > 0 && !affixEq(p, str, s, ci))) return false;
      const set = w.suffixSetStr;
      const runs = w.runs;
      const room = len - w.minLen;
      if (room < 0) return false;
      for (let r = 0; r < runs.length; r++) {
        const run = runs[r];
        if (run.len > room) break;
        if (!w.variable && run.len !== room) continue;
        if (runHas(set, run.start, run.end, str, t - run.len, run.len, ci)) return true;
      }
      return false;
    }
    default: {
      const nfa = w.nfa;
      if (nfa.countsBytes && !m.byteOnly && !isAscii(str, s, t)) {
        const b = utf8Latin1(str.slice(s, t));
        return nfa.matches(b, 0, b.length);
      }
      return nfa.matches(str, s, t);
    }
  }
}
