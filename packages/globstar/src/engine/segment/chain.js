// The chains: every path through the graph, matched by anchored
// compares when it has at most one globstar, and `matchDir` read off a
// lone chain. JS twin of `crates/globstar/src/engine/segment/chain.rs`.

import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  WK_AFFIX,
  WK_AFFIX_SET,
  WK_AFFIX_INDEX,
  MAX_CHAINS,
  NO_G,
  MANY_G,
  isGlobstar,
} from "./elem.js";
import { isPathSep, nextSep, lastSepBefore } from "../../bytes.js";
import { DirMatch, PRUNED } from "../../dir-match.js";
import { ACCEPT } from "./graph.js";
import { denseNfa, nfaRun } from "./nfa.js";
import { affixEq, litEq, wildConsumes } from "./wild.js";

// Every path through the graph as a chain, in pattern order, or none
// when there are more than MAX_CHAINS, or a path with several globstars
// is the only path or has more than 32 states (the NFA takes those).
export function chains(graph) {
  if (graph.linear) {
    const chain = chainOf(graph, null, true);
    return chain === null ? [] : [chain];
  }
  // Depth-first, first branch first, along one path: a level per node
  // on it (the start's below them) and the successor it tries next. A
  // path is copied only when it reaches the end.
  const paths = [];
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
      paths.push(path.slice());
      if (paths.length > MAX_CHAINS) return [];
      continue;
    }
    path.push(id);
    succs.push(graph.succs[id]);
    next.push(0);
  }
  const out = [];
  for (const nodes of paths) {
    const chain = chainOf(graph, nodes, paths.length === 1);
    if (chain === null) return [];
    out.push(chain);
  }
  return out;
}

// The chain of a path (`null`: the lone path of a brace-free pattern,
// whose nodes are `0..elems.length`), or `null` when it has several
// globstars and is the graph's `lone` path (the graph's NFA serves) or
// its NFA would not fit an int32.
function chainOf(graph, nodes, lone) {
  const elems = graph.elems;
  const n = nodes === null ? elems.length : nodes.length;
  const elem = (i) => elems[nodes === null ? i : nodes[i]];
  let globstar = NO_G;
  for (let i = 0; i < n; i++) {
    if (!isGlobstar(elem(i))) continue;
    if (globstar !== NO_G) {
      globstar = MANY_G;
      break;
    }
    globstar = i;
  }

  // Several globstars: the NFA over this path alone.
  let nfa = null;
  if (globstar === MANY_G) {
    if (lone || n + 1 > 32) return null;
    nfa = denseNfa(graph, nodes);
  }
  // A chain's own NFA keeps the chain's elements: no node list then.
  const chainNodes = nfa === null ? nodes : null;
  // A lone chain runs as it is; a union's chains also carry what a path
  // must start and end with, read off their first and last elements:
  // cheap rejects before a chain runs (`_anyChainMatches`).
  if (lone) return { nodes: chainNodes, globstar, nfa };
  const first = elem(0);
  const last = elem(n - 1);
  const quickHead = first.kind === EL_LIT ? first.litStr.charCodeAt(0) : -1;
  let quickSuffixStr = "";
  let quickPrefixStr = "";
  // A 64-slot sketch (two int32s, by `code & 63`) of a set's last
  // characters: a path ending in a character outside it matches none.
  // All ones when a suffix is empty, as anything ends it.
  let quickLastLo = -1;
  let quickLastHi = -1;
  if (last.kind === EL_LIT) quickSuffixStr = last.litStr;
  else if (last.kind === EL_WILD) {
    const w = last.wild;
    if (w.kind === WK_AFFIX) {
      if (w.suffixStr.length > 0) quickSuffixStr = w.suffixStr;
      else quickPrefixStr = w.prefixStr;
    } else if (w.kind === WK_AFFIX_SET || w.kind === WK_AFFIX_INDEX) {
      const set = w.suffixSetStr;
      quickLastLo = 0;
      quickLastHi = 0;
      for (let i = 0; i < set.length && quickLastLo !== -1; i++) {
        const s = set[i];
        if (s.length === 0) quickLastLo = quickLastHi = -1;
        else {
          const c = s.charCodeAt(s.length - 1) & 63;
          if (c < 32) quickLastLo |= 1 << c;
          else quickLastHi |= 1 << (c - 32);
        }
      }
    }
  }
  return {
    nodes: chainNodes,
    globstar,
    nfa,
    quickHead,
    quickSuffixStr,
    quickPrefixStr,
    quickLastLo,
    quickLastHi,
  };
}

// A chain's element: `ids === null` is the identity (the lone path of
// a brace-free pattern).
const elemAt = (elems, ids, i) => (ids === null ? elems[i] : elems[ids[i]]);
const chainLen = (elems, ids) => (ids === null ? elems.length : ids.length);

// The element compares `chainDir` spends on tail tokens; past them it
// answers what is safe ("may match below", "not all below"). Tails of
// up to 90 elements stay exact.
const MAX_TAIL_CHECKS = 4096;

// Does the chain match the whole path?
export function chainMatches(m, chain, str) {
  const ids = chain.nodes;
  switch (chain.globstar) {
    case NO_G:
      return fixedRun(m, ids, 0, chainLen(m.elems, ids), str, 0);
    case MANY_G: {
      // The NFA over this chain alone.
      const g = chain.nfa;
      return (nfaRun(m, g, str) & (1 << g.accept)) !== 0;
    }
    default: {
      const midStart = head(m, ids, chain.globstar, str);
      return midStart >= 0 && tailAndGlobstar(m, ids, chain.globstar, str, midStart);
    }
  }
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

// Do the elements `from..to` take the segments from `pos` to the end,
// one each?
function fixedRun(m, ids, from, to, str, pos) {
  const elems = m.elems;
  for (let i = from; i < to - 1; i++) {
    const end = consumeAt(m, elemAt(elems, ids, i), str, pos, false);
    if (end < 0) return false;
    pos = end + 1;
  }
  return consumeAt(m, elemAt(elems, ids, to - 1), str, pos, true) >= 0;
}

// The head (the elements before the globstar `g`), one segment each:
// where the segment after it starts, or -1. Every globstar
// wants that segment (G1 absorbs one, G0 is followed by its tail), so
// a path that ends in the head fails.
function head(m, ids, g, str) {
  const elems = m.elems;
  let pos = 0;
  for (let i = 0; i < g; i++) {
    const end = consumeAt(m, elemAt(elems, ids, i), str, pos, false);
    if (end < 0) return -1;
    pos = end + 1;
  }
  return pos;
}

// With the head consumed up to `midStart`: the tail on the last
// segments, found backward, and the globstar on what lies between.
function tailAndGlobstar(m, ids, g, str, midStart) {
  const elems = m.elems;
  const n = chainLen(elems, ids);
  // Where the tail starts; past the end when there is none.
  let split = str.length + 1;
  let end = str.length;
  for (let j = n - 1; j > g; j--) {
    const s = lastSepBefore(str, end) + 1;
    if (s < midStart || !elemConsumes(m, elemAt(elems, ids, j), str, s, end)) return false;
    split = s;
    if (j > g + 1) {
      if (s === 0) return false;
      end = s - 1;
    }
  }
  return absorbs(m, elemAt(elems, ids, g).kind, str, midStart, split);
}

// Can the globstar `kind` take the segments between the head (ending
// before `from`) and the tail starting at `split`? `split === from`
// means no segment at all. `dotAt`, when given, is `firstDot(m, str,
// from)`, which makes the dot check one compare.
function absorbs(m, kind, str, from, split, dotAt) {
  if (split === from) return kind === EL_G0;
  // Up to the separator before the tail, or the end.
  const to = split - 1;
  return dotAt === undefined ? dotFree(m, str, from, to) : dotAt >= to;
}

// Under `dot=false`, where the first segment of `str` from `from` on
// that starts with a `.` starts; Infinity when none does.
function firstDot(m, str, from) {
  if (m.dot) return Infinity;
  if (str.charCodeAt(from) === 0x2e) return from;
  for (let i = from; ; i++) {
    i = nextSep(str, i);
    if (i === -1) return Infinity;
    if (str.charCodeAt(i + 1) === 0x2e) return i + 1;
  }
}

// Under `dot=false`, no segment within `str[from, to)` may start with
// a `.`.
function dotFree(m, str, from, to) {
  if (m.dot) return true;
  if (from < to && str.charCodeAt(from) === 0x2e) return false;
  for (let i = from; ; ) {
    i = nextSep(str, i);
    if (i === -1 || i + 1 >= to) return true;
    if (str.charCodeAt(i + 1) === 0x2e) return false;
    i += 1;
  }
}

// `matchDir` through the lone chain of a matcher that keeps no NFA.
// The tokens the NFA would
// hold after `str`, read off the chain: a head position when the
// directory ends inside the head, the globstar when it has taken
// everything after the head, and a tail position when the tail's
// first elements take the last segments.
export function chainDir(m, str) {
  const elems = m.elems;
  const chain = m.chains[0];
  const ids = chain.nodes;
  const n = chainLen(elems, ids);
  const g = chain.globstar === NO_G ? n : chain.globstar;
  const tl = g < n ? n - g - 1 : 0;
  // Every element from `openFrom` on takes some segment: a token there
  // still reaches accept; from `anyFrom` on, any non-empty one.
  const openFrom = m.openFrom;
  const tailAny = n - tl >= m.anyFrom;
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
  const exact = tailAndGlobstar(m, ids, g, str, midStart);
  // The globstar took everything after the head.
  const gAlive = absorbs(m, kind, str, midStart, str.length + 1);
  let prefix = gAlive && g + 1 >= openFrom;
  let all = m.dot && gAlive && tailAny;
  // Tail tokens: the tail's first `j` elements on the last `j` segments
  // with the globstar released before them. A dead globstar may leave
  // one; `all` needs them all. Each `j` is its own alignment, so the
  // compares are budgeted.
  if ((!prefix || all) && tl > 1) {
    const dotAt = firstDot(m, str, midStart);
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
  return DirMatch.from(exact, prefix, all);
}

function elemConsumes(m, e, str, s, t) {
  if (e.kind === EL_LIT) return litEq(e.litStr, str, s, t, m.ci);
  if (e.kind === EL_WILD) return wildConsumes(m, e.wild, str, s, t);
  return false;
}
