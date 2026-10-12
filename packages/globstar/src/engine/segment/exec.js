// Matching over the segment graph. JS twin of `crates/globstar/src/
// engine/segment/exec.rs`: anchored chains for a path with at most one
// globstar, and the NFA over the graph otherwise (int32 active set up
// to 31 states, Int32Array words beyond).

import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_G0_STRICT,
  WK_AFFIX,
  WK_AFFIX_SET,
  WK_AFFIX_INDEX,
  NO,
  YES,
  BAIL,
  NO_G,
  MANY_G,
  elemSatisfiable,
  elemAnySeg,
} from "./elem.js";
import { isPathSep, eqByteCi, IS_WINDOWS_SEP, ctz32 } from "../../bytes.js";
import { DirMatch, PRUNED } from "../../dir-match.js";
import { bitTest, bitsOr, bitsIsZero, bitsEqual, bitsForEach } from "./bits.js";

export function chainMatches(m, chain, str, bail) {
  const ids = chain.nodes;
  switch (chain.globstar) {
    case NO_G:
      return fixedRun(m, ids, 0, chainLen(m.nodes, ids), str, 0, bail);
    case MANY_G: {
      // The NFA over this chain alone, or the matcher's when the
      // pattern is that one chain.
      const g = chain.nfa;
      if (g === null) return m._nfaMatch(str, bail);
      const active = nfaRun(m, g, str, bail);
      if (active === -1) return BAIL;
      return (active & (1 << g.accept)) !== 0 ? YES : NO;
    }
    default: {
      const midStart = head(m, ids, chain.globstar, str, bail);
      if (midStart < 0) return fail(midStart);
      return tailAndGlobstar(m, ids, chain.globstar, str, midStart, bail);
    }
  }
}

// A chain's element: `ids === null` is the identity (the lone path of
// a brace-free pattern).
const elemAt = (nodes, ids, i) => (ids === null ? nodes[i] : nodes[ids[i]]);
const chainLen = (nodes, ids) => (ids === null ? nodes.length : ids.length);

function nextSep(str, from) {
  const i = str.indexOf("/", from);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.indexOf("\\", from);
  if (i === -1) return j;
  if (j === -1) return i;
  return i < j ? i : j;
}

function lastSepBefore(str, end) {
  if (end <= 0) return -1;
  const i = str.lastIndexOf("/", end - 1);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.lastIndexOf("\\", end - 1);
  return i > j ? i : j;
}

// `consumeAt` results below zero: the element rejects the segment, or
// string mode must bail.
const MISS = -1;
const BAILED = -2;
const fail = (end) => (end === BAILED ? BAIL : NO);

// Element `e` on the segment at `pos`, the path's last one iff `last`:
// the index just past it. The segment is bounded first, so a path of
// the wrong depth is rejected before any wildcard runs; a literal is
// then compared in place.
function consumeAt(m, e, str, pos, last, bail) {
  if (e.kind === EL_LIT) {
    const end = pos + e.litStr.length;
    const bounded = last ? end === str.length : end < str.length && isPathSep(str.charCodeAt(end));
    return bounded && affixEq(e.litStr, str, pos, m.ci) === YES ? end : MISS;
  }
  if (e.kind !== EL_WILD) return MISS;
  let end = nextSep(str, pos);
  if (end === -1) end = str.length;
  if ((end === str.length) !== last) return MISS;
  const r = wildConsumes(e.wild, str, pos, end, m.ci, bail);
  return r === YES ? end : r === NO ? MISS : BAILED;
}

// Do the elements `from..to` take the segments from `pos` to the end,
// one each?
function fixedRun(m, ids, from, to, str, pos, bail) {
  const nodes = m.nodes;
  for (let i = from; i < to - 1; i++) {
    const end = consumeAt(m, elemAt(nodes, ids, i), str, pos, false, bail);
    if (end < 0) return fail(end);
    pos = end + 1;
  }
  const end = consumeAt(m, elemAt(nodes, ids, to - 1), str, pos, true, bail);
  return end < 0 ? fail(end) : YES;
}

// The head (the elements before the globstar `g`), one segment each:
// where the segment after it starts, or MISS/BAILED. Every globstar
// wants that segment (G1 absorbs one, G0 is followed by its tail), so
// a path that ends in the head fails.
function head(m, ids, g, str, bail) {
  const nodes = m.nodes;
  let pos = 0;
  for (let i = 0; i < g; i++) {
    const end = consumeAt(m, elemAt(nodes, ids, i), str, pos, false, bail);
    if (end < 0) return end;
    pos = end + 1;
  }
  return pos;
}

// With the head consumed up to `midStart`: the tail on the last
// segments, found backward, and the globstar on what lies between.
function tailAndGlobstar(m, ids, g, str, midStart, bail) {
  const nodes = m.nodes;
  const n = chainLen(nodes, ids);
  // Where the tail starts; past the end when there is none.
  let split = str.length + 1;
  let end = str.length;
  for (let j = n - 1; j > g; j--) {
    const s = lastSepBefore(str, end) + 1;
    if (s < midStart) return NO;
    const r = elemConsumes(elemAt(nodes, ids, j), str, s, end, m.ci, bail);
    if (r !== YES) return r;
    split = s;
    if (j > g + 1) {
      if (s === 0) return NO;
      end = s - 1;
    }
  }
  return absorbs(m, elemAt(nodes, ids, g).kind, str, midStart, split) ? YES : NO;
}

// Can the globstar `kind` take the segments between the head (ending
// before `from`) and the tail starting at `split`? `split === from`
// means no segment at all.
function absorbs(m, kind, str, from, split) {
  if (split === from) return kind === EL_G0 || kind === EL_G0_STRICT;
  // Up to the separator before the tail, or the end.
  const to = split - 1;
  // G0_STRICT may not start with an empty segment.
  if (kind === EL_G0_STRICT && (from === to || isPathSep(str.charCodeAt(from)))) return false;
  return dotFree(m, str, from, to);
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

// `matchDir` through the lone chain of a matcher that keeps no NFA:
// the DirMatch, or -1 on a string-mode bail. The tokens the NFA would
// hold after `str`, read off the chain: a head position when the
// directory ends inside the head, the globstar when it has taken
// everything after the head, and a tail position when the tail's
// first elements take the last segments.
export function chainDir(m, str, bail) {
  const nodes = m.nodes;
  const chain = m.chains[0];
  const ids = chain.nodes;
  const n = chainLen(nodes, ids);
  const g = chain.globstar === NO_G ? n : chain.globstar;
  const tl = g < n ? n - g - 1 : 0;
  // Every element from `i` on takes some segment: a token there still
  // reaches accept.
  const open = (i) => {
    for (let j = i; j < n; j++) if (!elemSatisfiable(elemAt(nodes, ids, j))) return false;
    return true;
  };
  let tailAny = true;
  for (let j = n - tl; j < n; j++) if (!elemAnySeg(elemAt(nodes, ids, j))) tailAny = false;
  if (str.length === 0) {
    // The walk root: nothing consumed yet. All below iff a leading
    // globstar is followed by at most one any-segment.
    const all = m.dot && g === 0 && tl <= 1 && tailAny;
    return DirMatch.fromExactPrefixAll(m.isMatch(str), true, all);
  }
  let midStart = 0;
  for (let i = 0; i < g; i++) {
    const e = elemAt(nodes, ids, i);
    let end = consumeAt(m, e, str, midStart, false, bail);
    if (end === BAILED) return -1;
    if (end === MISS) {
      end = consumeAt(m, e, str, midStart, true, bail);
      if (end === BAILED) return -1;
      if (end === MISS) return PRUNED;
      // `str` is the chain's first `k` segments. A globstar reached with
      // nothing absorbed covers all below iff its tail is at most one
      // any-segment.
      const k = i + 1;
      const all = m.dot && g < n && k === g && tl <= 1 && tailAny;
      return DirMatch.fromExactPrefixAll(k === n, k < n && open(k), all);
    }
    midStart = end + 1;
  }
  // A fixed chain, and the directory is deeper.
  if (g === n) return PRUNED;
  const kind = elemAt(nodes, ids, g).kind;
  const exact = tailAndGlobstar(m, ids, g, str, midStart, bail);
  if (exact === BAIL) return -1;
  // The globstar took everything after the head.
  const gAlive = absorbs(m, kind, str, midStart, str.length + 1);
  let prefix = gAlive && open(g + 1);
  let all = m.dot && gAlive && tailAny;
  // Tail tokens: the tail's first `j` elements on the last `j` segments
  // with the globstar released before them. A dead globstar may leave
  // one; `all` needs them all.
  if ((!prefix || all) && tl > 1) {
    let end = str.length;
    for (let j = 1; j < tl; j++) {
      const start = lastSepBefore(str, end) + 1;
      let alive = absorbs(m, kind, str, midStart, start);
      if (alive) {
        const r = fixedRun(m, ids, g + 1, g + 1 + j, str, start, bail);
        if (r === BAIL) return -1;
        alive = r === YES;
      }
      prefix ||= alive && open(g + 1 + j);
      all &&= alive;
      if (start === midStart) {
        // No segment is left for a longer tail.
        all &&= j + 1 === tl;
        break;
      }
      end = start - 1;
    }
  }
  return DirMatch.fromExactPrefixAll(exact === YES, prefix, all);
}

// ── graph NFA, int32 active set ─────────────────────────────────────

// The active set after every segment of `str`, or -1 on a string-mode
// bail.
export function nfaRun(m, g, str, bail) {
  let active = g.init;
  let pos = 0;
  for (;;) {
    if (active === 0) return 0;
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    active = nfaStep(m, g, active, str, pos, end, bail);
    if (active === -1) return -1;
    if (last) return active;
    pos = end + 1;
  }
}

function nfaStep(m, g, active, str, s0, e0, bail) {
  const nodes = m.nodes;
  const tab = g.tab;
  const ci = m.ci;
  let next = 0;
  const segEmpty = e0 === s0;
  const segDotLed = !segEmpty && str.charCodeAt(s0) === 0x2e;
  const absorbOk = m.dot || !segDotLed;
  let bits = active & ~(1 << g.accept);
  while (bits !== 0) {
    const s = ctz32(bits);
    bits &= bits - 1;
    const ni = tab[(s << 1) | 1];
    const e = nodes[ni];
    let consumes;
    switch (e.kind) {
      case EL_LIT:
        consumes = litEq(e.litStr, str, s0, e0, ci) === YES;
        break;
      case EL_WILD: {
        const r = wildConsumes(e.wild, str, s0, e0, ci, bail);
        if (r === BAIL) return -1;
        consumes = r === YES;
        break;
      }
      case EL_G0_STRICT:
        // The entry state is the first of the node's two and may not
        // take an empty segment.
        consumes = absorbOk && !(segEmpty && (s === 0 || tab[((s - 1) << 1) | 1] !== ni));
        break;
      default:
        consumes = absorbOk;
    }
    if (consumes) next |= tab[s << 1];
  }
  return next;
}

// `nfaStep` over a non-empty segment that only wildcards consume: every
// literal and class rejects it (`dot=true` only).
function nfaStepWild(m, active) {
  const g = m.graph;
  const nodes = m.nodes;
  const tab = g.tab;
  let next = 0;
  let bits = active & ~(1 << g.accept);
  while (bits !== 0) {
    const s = ctz32(bits);
    bits &= bits - 1;
    if (!elemAnySeg(nodes[tab[(s << 1) | 1]])) continue;
    next |= tab[s << 1];
  }
  return next;
}

// Does every path below the directory match? `active` is the state set
// the directory's segments leave behind.
//
// Feeds wildcard-only segments until the set settles, requiring an
// accept after each one. A real segment fires every transition a
// wildcard-only one does, so `true` holds for all real paths; `false`
// may only mean "not provable".
export function coversBelow(m, active) {
  const acceptBit = 1 << m.graph.accept;
  // Tokens only move forward or sit on a globstar, so the set settles
  // within the number of states.
  const limit = m.graph.tab.length >> 1;
  for (let step = 0; step <= limit; step++) {
    const next = nfaStepWild(m, active);
    if ((next & acceptBit) === 0) return false;
    if (next === active) return true;
    active = next;
  }
  return false;
}

// ── graph NFA, Int32Array active set ────────────────────────────────

// The active set after every segment of `str`, or null on a string-mode
// bail.
export function nfaRunWide(m, str, bail) {
  const g = m.graph;
  let active = g.init.slice();
  let next = new Int32Array(g.words);
  let pos = 0;
  for (;;) {
    if (bitsIsZero(active)) return active;
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    if (!nfaStepWide(m, active, next, str, pos, end, bail)) return null;
    const t = active;
    active = next;
    next = t;
    if (last) return active;
    pos = end + 1;
  }
}

function nfaStepWide(m, active, next, str, s0, e0, bail) {
  const g = m.graph;
  const nodes = m.nodes;
  const ci = m.ci;
  next.fill(0);
  const segEmpty = e0 === s0;
  const segDotLed = !segEmpty && str.charCodeAt(s0) === 0x2e;
  const absorbOk = m.dot || !segDotLed;
  let bailed = false;
  bitsForEach(active, (s) => {
    if (s === g.accept || bailed) return;
    const ni = g.nodeOf[s];
    const e = nodes[ni];
    let consumes;
    switch (e.kind) {
      case EL_LIT:
        consumes = litEq(e.litStr, str, s0, e0, ci) === YES;
        break;
      case EL_WILD: {
        const r = wildConsumes(e.wild, str, s0, e0, ci, bail);
        if (r === BAIL) {
          bailed = true;
          return;
        }
        consumes = r === YES;
        break;
      }
      case EL_G0_STRICT:
        consumes = absorbOk && !(segEmpty && (s === 0 || g.nodeOf[s - 1] !== ni));
        break;
      default:
        consumes = absorbOk;
    }
    if (consumes) bitsOr(next, g.after[s]);
  });
  return !bailed;
}

function nfaStepWildWide(m, active, next) {
  const g = m.graph;
  const nodes = m.nodes;
  next.fill(0);
  bitsForEach(active, (s) => {
    if (s === g.accept || !elemAnySeg(nodes[g.nodeOf[s]])) return;
    bitsOr(next, g.after[s]);
  });
}

export function coversBelowWide(m, active) {
  const g = m.graph;
  let next = new Int32Array(g.words);
  for (let step = 0; step <= g.after.length; step++) {
    nfaStepWildWide(m, active, next);
    if (!bitTest(next, g.accept)) return false;
    if (bitsEqual(next, active)) return true;
    const t = active;
    active = next;
    next = t;
  }
  return false;
}

// ── element predicates ──────────────────────────────────────────────

function elemConsumes(e, str, s, t, ci, bail) {
  if (e.kind === EL_LIT) return litEq(e.litStr, str, s, t, ci);
  if (e.kind === EL_WILD) return wildConsumes(e.wild, str, s, t, ci, bail);
  return NO;
}

function litEq(lit, str, s, t, ci) {
  if (t - s !== lit.length) return NO;
  if (!ci) return str.startsWith(lit, s) ? YES : NO;
  for (let i = 0; i < lit.length; i++) {
    if (!eqByteCi(lit.charCodeAt(i), str.charCodeAt(s + i))) return NO;
  }
  return YES;
}

export function affixEq(part, str, at, ci) {
  if (!ci) return str.startsWith(part, at) ? YES : NO;
  for (let i = 0; i < part.length; i++) {
    if (!eqByteCi(part.charCodeAt(i), str.charCodeAt(at + i))) return NO;
  }
  return YES;
}

function segHasNonAscii(str, s, t) {
  for (let i = s; i < t; i++) {
    if (str.charCodeAt(i) > 0x7f) return true;
  }
  return false;
}

// Binary search of `set[lo, hi)` (sorted, one length) for the segment
// tail `str[at, at + len)`, folding the tail's ASCII case when `ci`.
function setHas(set, lo, hi, str, at, len, ci) {
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

function wildConsumes(w, str, s, t, ci, bail) {
  if (w.dotProtect && t > s && str.charCodeAt(s) === 0x2e) return NO;
  const len = t - s;
  switch (w.kind) {
    case WK_AFFIX: {
      if (bail && w.anychars > 0 && segHasNonAscii(str, s, t)) return BAIL;
      const need = w.minLen;
      if (len < need || (!w.variable && len !== need)) return NO;
      if (w.prefixStr.length > 0 && affixEq(w.prefixStr, str, s, ci) === NO) return NO;
      if (w.suffixStr.length > 0 && affixEq(w.suffixStr, str, t - w.suffixStr.length, ci) === NO) {
        return NO;
      }
      return YES;
    }
    case WK_AFFIX_SET: {
      if (bail && w.anychars > 0 && segHasNonAscii(str, s, t)) return BAIL;
      const p = w.prefixStr;
      if (len < p.length || (p.length > 0 && affixEq(p, str, s, ci) === NO)) return NO;
      const set = w.suffixSetStr;
      for (let i = 0; i < set.length; i++) {
        const suf = set[i];
        const need = w.minLen + suf.length;
        if (len < need || (!w.variable && len !== need)) continue;
        if (suf.length === 0 || affixEq(suf, str, t - suf.length, ci) !== NO) return YES;
      }
      return NO;
    }
    case WK_AFFIX_INDEX: {
      if (bail && w.anychars > 0 && segHasNonAscii(str, s, t)) return BAIL;
      const p = w.prefixStr;
      if (len < p.length || (p.length > 0 && affixEq(p, str, s, ci) === NO)) return NO;
      const set = w.suffixSetStr;
      const runs = w.runs;
      const room = len - w.minLen;
      if (room < 0) return NO;
      for (let r = 0; r < runs.length; r++) {
        const run = runs[r];
        if (run.len > room) break;
        if (!w.variable && run.len !== room) continue;
        if (setHas(set, run.start, run.end, str, t - run.len, run.len, ci)) return YES;
      }
      return NO;
    }
    default: {
      const nfa = w.nfa;
      if (bail && nfa.needsAsciiSeg && segHasNonAscii(str, s, t)) return BAIL;
      return nfa.matches(str, s, t) ? YES : NO;
    }
  }
}
