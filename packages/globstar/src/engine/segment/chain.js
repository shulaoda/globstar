// The chains: each path through the graph, matched by anchored compares
// when it has at most one globstar or two around a block; the literal-only
// paths as one sorted set; `matchDir` read off a lone chain.

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
import { WK_AFFIX, WK_SUFFIXES, wildConsumes } from "./wild.js";
import {
  IS_WINDOWS_SEP,
  affixEq,
  asciiLower,
  cmpFolded,
  isPathSep,
  litEq,
  nextSep,
  lastSepBefore,
} from "../../bytes.js";
import { flat } from "../../utf8.js";
import { DirMatch, PRUNED } from "../../dir-match.js";

// Wildcard chains kept without an index; more while their nodes fit four
// times the pattern's weight.
export const MAX_CHAINS = 64;

// The literal paths' set: this many characters, or four times the
// pattern's weight.
const MAX_LITERAL_BYTES = 64 * 1024;

// `chain.globstar` when the chain has none, or is the literal paths' set.
const NO_G = -1;
export const SET_G = -2;

// Element compares `chainDir` spends on tail tokens; past them it answers
// what is safe. Tails of up to 90 elements stay exact.
const MAX_TAIL_CHECKS = 4096;

// `ids === null`: the lone path of a linear pattern, whose nodes are
// `0..elems.length`.
const elemAt = (elems, ids, i) => (ids === null ? elems[i] : elems[ids[i]]);
const chainLen = (elems, ids) => (ids === null ? elems.length : ids.length);

// Every path through the graph as a chain, in pattern order, the literal
// ones as one SET_G chain first; none when they pass the caps, a path has
// more than two globstars, or a brace cuts a segment.
export function chainsOf(graph, ci) {
  if (graph.glue !== null) return [];
  if (graph.linear) {
    const chain = chainOf(graph, null, true, ci);
    return chain === null ? [] : [chain];
  }
  // Depth-first along one path: a level per node on it (the start's below
  // them) and the successor it tries next.
  const paths = [];
  let pathNodes = 0;
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
        pathNodes += nodes.length;
        paths.push(exact(nodes));
      }
      if (
        (paths.length > MAX_CHAINS && pathNodes > 4 * graph.weight) ||
        literalChars + literalPaths.length > Math.max(MAX_LITERAL_BYTES, 4 * graph.weight)
      ) {
        return [];
      }
      continue;
    }
    path.push(id);
    stack.push(graph.succs[id]);
    nextAt.push(0);
  }
  const out = [];
  if (literalPaths.length > 0) {
    literalPaths.sort();
    const set = literalPaths.filter((s, i) => i === 0 || s !== literalPaths[i - 1]);
    out.push({ nodes: null, globstar: SET_G, literals: set.map(flat) });
  }
  const lone = paths.length === 1 && literalPaths.length === 0;
  for (const nodes of paths) {
    const chain = chainOf(graph, nodes, lone, ci);
    if (chain === null) return [];
    out.push(chain);
  }
  return out;
}

// The chain of a path, or null when it has more than two globstars, or
// two with nothing between. A lone chain carries what `chainDir` reads
// off it; a union's chains carry their gate, exact tests a
// case-insensitive chain does without.
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
    // Every element from `openFrom` on takes some segment, from `anyFrom`
    // on any non-empty one.
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
  // A 64-slot sketch of a set's last characters (two int32s, by
  // `code & 63`); all ones when a suffix is empty.
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
    case SET_G:
      return hasLiteral(m, chain.literals, str);
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

// Compared as the keys were folded: letters lowercased when
// case-insensitive, separators as `/`.
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

// Element `e` on the segment at `pos`, the path's last one iff `last`: the
// index just past it, or -1. The segment is bounded before a literal is
// compared in place.
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

// Do the elements `from..to` take the segments from `pos` to the end, one
// each?
function fixedRun(m, ids, from, to, str, pos) {
  pos = run(m, ids, from, to - 1, str, pos);
  return pos >= 0 && consumeAt(m, elemAt(m.elems, ids, to - 1), str, pos, true) >= 0;
}

// The elements `from..to` on the segments from `pos`, one each, each
// followed by a separator: where the segment after them starts, or -1.
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

// The tail after element `g` on the last segments, found backward: where
// it starts, past the end when there is none; -1 when it does not fit
// after `midStart`.
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

// The block between the globstars `g1` and `g2` on some run of the
// segments between the head and the tail, each globstar on what is left
// on its side. Under `dot=false` a globstar takes no dot segment, so the
// block must start at or before the first one and none may follow it.
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

// The elements `from..to` on the segments from `pos`, the first of which
// ends at `end`, one each, the last ending at a separator or at `midEnd`:
// the index just past it, or -1.
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

// Can the globstar `kind` take the segments from `from` to the tail at
// `split`? `split === from`: no segment at all. `dotAt`, when given, is
// `firstDot(m, str, from)`.
function absorbs(m, kind, str, from, split, dotAt) {
  if (split === from) return kind === EL_G0;
  const to = split - 1;
  return m.dot || (dotAt === undefined ? firstDot(m, str, from, to) : dotAt) >= to;
}

// Where the first dot-led segment of `str[from, to)` starts; Infinity
// when none does, and under `dot`.
function firstDot(m, str, from, to) {
  if (m.dot) return Infinity;
  if (from < to && str.charCodeAt(from) === 0x2e) return from;
  for (let i = from; ; i++) {
    i = nextSep(str, i);
    if (i === -1 || i + 1 >= to) return Infinity;
    if (str.charCodeAt(i + 1) === 0x2e) return i + 1;
  }
}

// `matchDir` through a lone chain: the tokens the NFA would hold after
// `str`, read off the chain.
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
      // `str` is the chain's first `k` segments.
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
  // Tail tokens: the tail's first `j` elements on the last `j` segments,
  // the globstar released before them. Each `j` is its own alignment, so
  // the compares are budgeted.
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
