// The chains: every path through the graph, matched by anchored
// compares when it has at most one globstar or two around a block, the
// literal-only paths as one sorted set, and `matchDir` read off a lone
// chain. JS twin of `crates/globstar/src/engine/segment/chain.rs`.

import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_JOIN,
  WK_AFFIX,
  WK_SUFFIXES,
  elemAnySeg,
  elemSatisfiable,
  exact,
  isGlobstar,
} from "./elem.js";
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
import { ACCEPT } from "./graph.js";
import { wildConsumes } from "./wild.js";

// ── the chains ──────────────────────────────────────────────────────────

// Paths with a wildcard `isMatch` tries one by one; past that the NFA
// over the whole graph runs.
const MAX_CHAINS = 64;

// The literal-only paths kept as one set (a union of file names), and
// the characters it may take, or four times the pattern's weight (a
// long literal behind crossing braces is copied once per path); past
// either the NFA over the whole graph runs.
const MAX_LITERALS = 4096;
const MAX_LITERAL_BYTES = 64 * 1024;

// `chain.globstar` when the chain has none, or is the literal paths'
// set.
const NO_G = -1;
export const SET_G = -2;

// The element compares `chainDir` spends on tail tokens; past them it
// answers what is safe ("may match below", "not all below"). Tails of
// up to 90 elements stay exact.
const MAX_TAIL_CHECKS = 4096;

// A chain's element: `ids === null` is the identity (the lone path of
// a linear pattern).
const elemAt = (elems, ids, i) => (ids === null ? elems[i] : elems[ids[i]]);
const chainLen = (elems, ids) => (ids === null ? elems.length : ids.length);

// ── building ────────────────────────────────────────────────────────────

// Every path through the graph as a chain, in pattern order, the literal
// ones together as one `SET_G` chain first; none when the others pass
// MAX_CHAINS or the literal ones MAX_LITERALS, or a path has more than
// two globstars (the NFA takes those).
export function chains(graph, ci) {
  if (graph.linear) {
    const chain = chainOf(graph, null, true, ci);
    return chain === null ? [] : [chain];
  }
  // Depth-first, first branch first, along one path: a level per node
  // on it (the start's below them) and the successor it tries next. A
  // path is copied only when it reaches the end.
  const paths = [];
  // The literal paths, as the characters each matches, keyed as a path
  // is searched (`hasLiteral`).
  const lits = [];
  let litChars = 0;
  const path = [];
  const succs = [graph.start];
  const next = [0];
  while (succs.length > 0) {
    const top = succs.length - 1;
    if (next[top] === succs[top].length) {
      succs.pop();
      next.pop();
      path.pop();
      continue;
    }
    const id = succs[top][next[top]++];
    if (id === ACCEPT) {
      // A path of literal nodes joins the set as the characters it
      // matches; any other is a chain. Joins take no segment: they are
      // left out (and the chain copied to its length, see `exact`).
      const nodes = path.filter((i) => graph.elems[i].kind !== EL_JOIN);
      if (nodes.every((i) => graph.elems[i].kind === EL_LIT)) {
        const joined = nodes.map((i) => graph.elems[i].litStr).join("/");
        lits.push(ci ? asciiLower(joined) : joined);
        litChars += joined.length;
      } else paths.push(exact(nodes));
      if (
        paths.length > MAX_CHAINS ||
        lits.length > MAX_LITERALS ||
        litChars > Math.max(MAX_LITERAL_BYTES, 4 * graph.weight)
      ) {
        return [];
      }
      continue;
    }
    path.push(id);
    succs.push(graph.succs[id]);
    next.push(0);
  }
  const out = [];
  if (lits.length > 0) {
    lits.sort();
    const set = lits.filter((s, i) => i === 0 || s !== lits[i - 1]);
    out.push({ nodes: null, globstar: SET_G, literals: set.map(flat) });
  }
  const lone = paths.length === 1 && lits.length === 0;
  for (const nodes of paths) {
    const chain = chainOf(graph, nodes, lone, ci);
    if (chain === null) return [];
    out.push(chain);
  }
  return out;
}

// The chain of a path (`null`: the lone path of a linear pattern,
// whose nodes are `0..elems.length`), or `null` when it has more than
// two globstars, or two with nothing between (the graph's NFA serves).
function chainOf(graph, nodes, lone, ci) {
  const elems = graph.elems;
  const n = chainLen(elems, nodes);
  const elem = (i) => elemAt(elems, nodes, i);
  // The globstars: none, one (`globstar`), or two around a block of
  // elements (`globstar` and `g2`: `**/x/**`); more, or two with nothing
  // between, is no chain.
  let globstar = NO_G;
  let g2 = -1;
  for (let i = 0; i < n; i++) {
    if (!isGlobstar(elem(i))) continue;
    if (globstar === NO_G) globstar = i;
    else if (g2 < 0 && i > globstar + 1) g2 = i;
    else return null;
  }
  // A lone chain carries what `chainDir` reads off it: every element
  // from `openFrom` on takes some segment, from `anyFrom` on any
  // non-empty one. A union's chains carry what a path must start and
  // end with, read off their first and last elements: cheap rejects
  // before a chain runs (`_anyChainMatches`). They are exact tests, so
  // a case-insensitive chain carries none.
  if (lone) {
    // The first index from which every element passes `ok`.
    const from = (ok) => {
      let i = n;
      while (i > 0 && ok(elem(i - 1))) i--;
      return i;
    };
    return { nodes, globstar, g2, openFrom: from(elemSatisfiable), anyFrom: from(elemAnySeg) };
  }
  const first = elem(0);
  const last = elem(n - 1);
  const quickHead = !ci && first.kind === EL_LIT ? first.litStr.charCodeAt(0) : -1;
  let quickSuffixStr = "";
  let quickPrefixStr = "";
  // A 64-slot sketch (two int32s, by `code & 63`) of a set's last
  // characters: a path ending in a character outside it matches none.
  // All ones when a suffix is empty, as anything ends it.
  let quickLastLo = -1;
  let quickLastHi = -1;
  if (!ci && last.kind === EL_LIT) quickSuffixStr = last.litStr;
  else if (!ci && last.kind === EL_WILD) {
    const w = last.wild;
    if (w.kind === WK_AFFIX) {
      if (w.suffixStr.length > 0) quickSuffixStr = w.suffixStr;
      else quickPrefixStr = w.prefixStr;
    } else if (w.kind === WK_SUFFIXES && !w.suffixEmpty) {
      quickLastLo = 0;
      quickLastHi = 0;
      for (const s of w.suffixSetStr) {
        const c = s.charCodeAt(s.length - 1) & 63;
        if (c < 32) quickLastLo |= 1 << c;
        else quickLastHi |= 1 << (c - 32);
      }
    }
  }
  return {
    nodes,
    globstar,
    g2,
    quickHead,
    quickSuffixStr,
    quickPrefixStr,
    quickLastLo,
    quickLastHi,
  };
}

// ── matching ────────────────────────────────────────────────────────────

// Does the chain match the whole path?
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

// Is the path one of the literal paths? Compared as the keys were
// folded: ASCII letters lowercased when case-insensitive, separators as
// `/`; as they are otherwise, where the engine's own order serves.
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

// Element `e` on the segment at `pos`, the path's last one iff `last`:
// the index just past it, or -1. The segment is bounded first, so a path of
// the wrong depth is rejected before any wildcard runs; a literal is
// then compared in place.
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

// Do the elements `from..to` take the segments from `pos` to the end,
// one each?
function fixedRun(m, ids, from, to, str, pos) {
  pos = run(m, ids, from, to - 1, str, pos);
  return pos >= 0 && consumeAt(m, elemAt(m.elems, ids, to - 1), str, pos, true) >= 0;
}

// The elements `from..to` on the segments from `pos`, one each, each
// followed by a separator: where the segment after them starts, or -1.
// Every globstar wants that segment (G1 absorbs one, G0 is followed by
// its tail), so a path that ends in a head fails.
function run(m, ids, from, to, str, pos) {
  const elems = m.elems;
  for (let i = from; i < to; i++) {
    const end = consumeAt(m, elemAt(elems, ids, i), str, pos, false);
    if (end < 0) return -1;
    pos = end + 1;
  }
  return pos;
}

// With the head consumed up to `midStart`: the tail on the last
// segments, found backward, and the globstar on what lies between.
function tailAndGlobstar(m, ids, g, str, midStart) {
  const split = tailSplit(m, ids, g, str, midStart);
  return split >= 0 && absorbs(m, elemAt(m.elems, ids, g).kind, str, midStart, split);
}

// The tail after element `g` on the last segments, found backward:
// where it starts, past the end when there is none; -1 when it does not
// fit after `midStart`.
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

// With the head consumed up to `midStart` and the tail found at `split`:
// the block of elements between the globstars `g1` and `g2` on some run
// of the segments between, each globstar on what is left on its side.
// Under `dot=false` a globstar takes no dot segment, so the block must
// start at or before the first one and none may follow it.
function blockBetween(m, ids, g1, g2, str, midStart, split) {
  if (split === midStart) return false;
  // The middle's segments end before the tail's separator, or at the
  // end of the path (`split` is then one past it).
  const midEnd = split - 1;
  const elems = m.elems;
  // The first dot segment from where the second globstar starts. The
  // block only moves right, so it is looked for again only once the
  // block has passed it: one scan of the middle in all.
  let dotAt = 0;
  let pos = midStart;
  for (;;) {
    let end = nextSep(str, pos);
    if (end === -1) end = str.length;
    const taken = blockFrom(m, ids, g1 + 1, g2, str, pos, end, midEnd);
    if (taken >= 0) {
      // Nothing before the block holds a dot segment (the loop stops at
      // the first); the second globstar checks after.
      const left = pos > midStart || elemAt(elems, ids, g1).kind === EL_G0;
      const from = taken + 1;
      if (left) {
        if (dotAt < from) dotAt = firstDot(m, str, from, midEnd);
        if (absorbs(m, elemAt(elems, ids, g2).kind, str, from, split, dotAt)) return true;
      }
    }
    // Past the middle, or at a dot segment the first globstar could not
    // take.
    if (end >= midEnd || (!m.dot && pos < end && str.charCodeAt(pos) === 0x2e)) return false;
    pos = end + 1;
  }
}

// The elements `from..to` on the segments from `pos`, the first of which
// ends at `end`, one each, the last ending at a separator or at
// `midEnd`: the index just past it, or -1.
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

// Can the globstar `kind` take the segments between the head (ending
// before `from`) and the tail starting at `split`? `split === from`
// means no segment at all. `dotAt`, when given, is `firstDot(m, str,
// from)`, which makes the dot check one compare.
function absorbs(m, kind, str, from, split, dotAt) {
  if (split === from) return kind === EL_G0;
  // Up to the separator before the tail, or the end.
  const to = split - 1;
  return m.dot || (dotAt === undefined ? firstDot(m, str, from, to) : dotAt) >= to;
}

// Under `dot=false`, where the first segment of `str[from, to)` that
// starts with a `.` starts; Infinity when none does, and under `dot`.
function firstDot(m, str, from, to) {
  if (m.dot) return Infinity;
  if (from < to && str.charCodeAt(from) === 0x2e) return from;
  for (let i = from; ; i++) {
    i = nextSep(str, i);
    if (i === -1 || i + 1 >= to) return Infinity;
    if (str.charCodeAt(i + 1) === 0x2e) return i + 1;
  }
}

// ── matchDir ────────────────────────────────────────────────────────────

// `matchDir` through the lone chain of a matcher that keeps no NFA: the
// tokens the NFA would hold after `str`, read off the chain. A head
// position when the directory ends inside the head, the globstar when it
// has taken everything after the head, and a tail position when the
// tail's first elements take the last segments.
export function chainDir(m, str) {
  const elems = m.elems;
  const chain = m.chains[0];
  const ids = chain.nodes;
  const n = chainLen(elems, ids);
  const g = chain.globstar === NO_G ? n : chain.globstar;
  const tl = g < n ? n - g - 1 : 0;
  // Every element from `openFrom` on takes some segment: a token there
  // still reaches accept; from `anyFrom` on, any non-empty one.
  const openFrom = chain.openFrom;
  const tailAny = n - tl >= chain.anyFrom;
  if (str.length === 0) {
    // The walk root: nothing consumed yet. All below iff a leading
    // globstar is followed by at most one any-segment.
    const all = m.dot && g === 0 && tl <= 1 && tailAny;
    return DirMatch.from(m.isMatch(str), true, all);
  }
  let midStart = 0;
  for (let i = 0; i < g; i++) {
    const e = elemAt(elems, ids, i);
    let end = consumeAt(m, e, str, midStart, false);
    if (end < 0) {
      end = consumeAt(m, e, str, midStart, true);
      if (end < 0) return PRUNED;
      // `str` is the chain's first `k` segments. A globstar reached with
      // nothing absorbed covers all below iff its tail is at most one
      // any-segment.
      const k = i + 1;
      const all = m.dot && g < n && k === g && tl <= 1 && tailAny;
      return DirMatch.from(k === n, k < n && k >= openFrom, all);
    }
    midStart = end + 1;
  }
  // A fixed chain, and the directory is deeper.
  if (g === n) return PRUNED;
  const kind = elemAt(elems, ids, g).kind;
  const matched = tailAndGlobstar(m, ids, g, str, midStart);
  // The globstar took everything after the head.
  const gAlive = absorbs(m, kind, str, midStart, str.length + 1);
  let prefix = gAlive && g + 1 >= openFrom;
  let all = m.dot && gAlive && tailAny;
  // Tail tokens: the tail's first `j` elements on the last `j` segments
  // with the globstar released before them. A dead globstar may leave
  // one; `all` needs them all. Each `j` is its own alignment, so the
  // compares are budgeted.
  if ((!prefix || all) && tl > 1) {
    const dotAt = firstDot(m, str, midStart, str.length);
    let checks = MAX_TAIL_CHECKS;
    let end = str.length;
    for (let j = 1; j < tl; j++) {
      if (checks < j) {
        prefix = true;
        all = false;
        break;
      }
      checks -= j;
      const start = lastSepBefore(str, end) + 1;
      const alive =
        absorbs(m, kind, str, midStart, start, dotAt) &&
        fixedRun(m, ids, g + 1, g + 1 + j, str, start);
      prefix ||= alive && g + 1 + j >= openFrom;
      all &&= alive;
      if (start === midStart) {
        // No segment is left for a longer tail.
        all &&= j + 1 === tl;
        break;
      }
      if (prefix && !all) break;
      end = start - 1;
    }
  }
  return DirMatch.from(matched, prefix, all);
}
