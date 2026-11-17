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
  room,
} from "./graph.js";
import { WK_AFFIX, WK_SUFFIXES, wildConsumes, wildWeight } from "./wild.js";
import {
  IS_WINDOWS_SEP,
  affixEq,
  asciiLower,
  cmpFolded,
  isPathSep,
  litEq,
  nextSep,
  lastSepBefore,
  fold,
} from "../bytes.js";
import { DirMatch, PRUNED } from "../../dir-match.js";

const NO_G = -1;

const MAX_TAIL_CHECKS = 4096;

const elemAt = (elems, ids, i) => (ids === null ? elems[i] : elems[ids[i]]);
const chainLen = (elems, ids) => (ids === null ? elems.length : ids.length);

export function pathsOf(graph, ci) {
  const none = { literals: null, chains: [] };
  if (graph.glue !== null) return none;
  if (graph.linear) {
    const chain = chainOf(graph, null, true, ci);
    return chain === null ? none : { literals: null, chains: [chain] };
  }
  const paths = [];
  let pathWeight = 0;
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
        for (const i of nodes) {
          const e = graph.elems[i];
          pathWeight += e.kind === EL_WILD ? wildWeight(e.wild) : 1;
        }
        paths.push(exact(nodes));
      }
      if (
        pathWeight > room(graph.weight) ||
        literalChars + literalPaths.length > room(graph.weight)
      ) {
        return none;
      }
      continue;
    }
    path.push(id);
    stack.push(graph.succs[id]);
    nextAt.push(0);
  }
  const lone = paths.length === 1 && literalPaths.length === 0;
  const chains = [];
  for (const nodes of paths) {
    const chain = chainOf(graph, nodes, lone, ci);
    if (chain === null) return none;
    chains.push(chain);
  }
  let literals = null;
  if (literalPaths.length > 0) {
    literalPaths.sort();
    literals = literalPaths.filter((s, i) => i === 0 || s !== literalPaths[i - 1]);
  }
  return { literals, chains };
}

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
  const g = chain.globstar;
  if (g === NO_G) return fixedRun(m, ids, 0, chainLen(m.elems, ids), str, 0);
  const midStart = run(m, ids, 0, g, str, 0);
  if (midStart < 0) return false;
  if (chain.g2 < 0) return tailAndGlobstar(m, ids, g, str, midStart);
  const split = tailSplit(m, ids, chain.g2, str, midStart);
  return split >= 0 && blockBetween(m, ids, g, chain.g2, str, midStart, split);
}

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

export function anyBelow(m, literals, dir) {
  let lo = 0;
  let hi = literals.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (cmpBelow(literals[mid], dir, m.ci) < 0) lo = mid + 1;
    else hi = mid;
  }
  return lo < literals.length && cmpBelow(literals[lo], dir, m.ci) === 0;
}

function cmpBelow(lit, dir, ci) {
  const n = Math.min(lit.length, dir.length);
  for (let i = 0; i < n; i++) {
    const c = dir.charCodeAt(i);
    const d = lit.charCodeAt(i) - (IS_WINDOWS_SEP && c === 0x5c ? 0x2f : fold(c, ci));
    if (d !== 0) return d;
  }
  return lit.length > dir.length ? lit.charCodeAt(dir.length) - 0x2f : -1;
}

function consumeAt(m, e, str, pos, last) {
  if (e.kind === EL_LIT) {
    const end = pos + e.litStr.length;
    const bounded = last ? end === str.length : end < str.length && isPathSep(str.charCodeAt(end));
    return bounded && affixEq(e.litStr, str, pos, m.ci) ? end : -1;
  }
  let end = nextSep(str, pos);
  if (end === -1) end = str.length;
  if ((end === str.length) !== last) return -1;
  return wildConsumes(m, e.wild, str, pos, end) ? end : -1;
}

function elemConsumes(m, e, str, s, t) {
  if (e.kind === EL_LIT) return litEq(e.litStr, str, s, t, m.ci);
  return wildConsumes(m, e.wild, str, s, t);
}

function fixedRun(m, ids, from, to, str, pos) {
  pos = run(m, ids, from, to - 1, str, pos);
  return pos >= 0 && consumeAt(m, elemAt(m.elems, ids, to - 1), str, pos, true) >= 0;
}

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
      const after = blockEnd + 1;
      if (dotAt < after) dotAt = firstDot(m, str, after, midEnd);
      if (absorbs(m, elemAt(elems, ids, g2).kind, str, after, split, dotAt)) return true;
    }
    if (end >= midEnd || (!m.dot && pos < end && str.charCodeAt(pos) === 0x2e)) return false;
    pos = end + 1;
  }
}

function blockFrom(m, ids, from, to, str, pos, end, midEnd) {
  const elems = m.elems;
  if (!elemConsumes(m, elemAt(elems, ids, from), str, pos, end)) return -1;
  if (to - from === 1) return end;
  if (end >= midEnd) return -1;
  const p = run(m, ids, from + 1, to - 1, str, end + 1);
  if (p < 0) return -1;
  const taken = consumeAt(m, elemAt(elems, ids, to - 1), str, p, false);
  return taken <= midEnd ? taken : -1;
}

function absorbs(m, kind, str, from, split, dotAt) {
  if (split === from) return kind === EL_G0;
  const to = split - 1;
  return m.dot || (dotAt === undefined ? firstDot(m, str, from, to) : dotAt) >= to;
}

function firstDot(m, str, from, to) {
  if (m.dot) return Infinity;
  if (from < to && str.charCodeAt(from) === 0x2e) return from;
  for (let i = from; ; i++) {
    i = nextSep(str, i);
    if (i === -1 || i + 1 >= to) return Infinity;
    if (str.charCodeAt(i + 1) === 0x2e) return i + 1;
  }
}

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
