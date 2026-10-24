// In-segment mini NFA — the `WK_GENERIC` wild backend. JS port of the
// Rust module `crates/globstar/src/engine/segment/seg_nfa.rs`: a
// Thompson NFA over the in-segment nodes, simulated with an int32 bit set
// for up to 32 states and Int32Array words beyond, so no in-segment
// pattern is too large. One state per node: a `*` takes any unit and
// stays, with an ε-edge out.
//
// Every ε-edge of the NFA points to a later state (see `compileNode`), so
// one ascending sweep closes a set. A step ORs per-state closures while
// they fit MAX_CLOSURE_BYTES; past that (a dense set per state is
// quadratic in the states) it closes its set as it goes, so memory stays
// linear in the pattern.
//
// The same states match the pieces of the segments a separator-crossing
// brace cuts, glued together (`GlueNfa`).

import { N_ANYCHAR, N_BRACE, N_CLASS, N_LITERAL, N_STAR, lit, seq } from "../../parse/ast.js";
import { ctz32 } from "../../bytes.js";
import { isAscii, utf8Latin1 } from "../../utf8.js";
import { EL_JOIN, EL_LIT, EL_PIECE, EL_WILD, INNER, exact, ACCEPT } from "./graph.js";
import { wordsFor, bitTest, bitSet, bitsIsZero, byteSet, setEmpty, setHas } from "./bits.js";
import { NEVER_LEN } from "./wild.js";

// NFA state kinds. A `*` is an S_ANY whose `next` is itself, with its
// way out in `splitBs`: the hot loop treats it like `?` (a separate kind
// cost 5-20% there).
const S_BYTE = 0;
const S_CLASS = 1;
const S_ANY = 2;
const S_SPLIT = 3;
const S_JUMP = 4;
const S_MATCH = 5;
// A glued piece's segment may end here, at graph node `nexts[s]` (see
// `GlueNfa`): kept in a set as S_MATCH is.
const S_EXIT = 6;

const UNSET = -1;

// The most the per-state closures may take, about 1,000 states. Up to there
// ORing dense closures is the faster step; past it, closing the set as
// the step goes wins and keeps memory linear.
const MAX_CLOSURE_BYTES = 128 * 1024;

// The segment's NFA and what the graph reads off it, as `makeWild`
// fields: one that accepts nothing gets NEVER_LEN, and it is
// dot-protected when nothing that may take a protected leading `.` (a
// literal `.`, a positive class listing it) can start a match.
export function compileSegNfa(nodes, dot, ci) {
  const b = new SegBuilder(ci);
  const entry = b.compileNodes(nodes);
  const accept = b.alloc(S_MATCH, 0, UNSET);
  for (const t of b.tails) b.patch(t, accept);
  const small = b.kinds.length <= 32;
  const nfa = small ? new SegNfaSmall(b, entry, dot) : new SegNfaWide(b, entry, dot);
  const test = small ? (a, s) => (a & (1 << s)) !== 0 : bitTest;
  return {
    matcher: nfa,
    minLen: satisfiable(b, entry) ? 0 : NEVER_LEN,
    dotProtect: !dot && wildLed(b, nfa.initDotBlocked, test),
    anySeg: anySeg(b, entry, nfa.init, test),
  };
}

// A letter as the class of both its cases, or null. (Its own function:
// `litState` stays small enough to inline into the build loop.)
function letterClass(b) {
  const lower = b | 0x20;
  if (lower < 0x61 || lower > 0x7a) return null;
  const bits = [0, 0, 0, 0, 0, 0, 0, 0];
  for (const c of [lower - 0x20, lower]) bits[c >> 5] |= 1 << (c & 31);
  return { neg: false, bits };
}

// Shared post-construction facts: a protected leading `.` can only be
// taken by a literal `.` or a positive class containing it.
function wildLed(b, initDotBlocked, test) {
  const n = b.kinds.length;
  for (let s = 0; s < n; s++) {
    if (!test(initDotBlocked, s)) continue;
    if (b.kinds[s] === S_BYTE && b.byteVals[s] === 0x2e) return false;
    if (b.kinds[s] === S_CLASS && setHas(b.clsRefs[s], 0x2e)) return false;
  }
  return true;
}

// Can the entry reach S_MATCH through consumers some byte fires? Every
// edge but a star's loop onto itself points to a later state, so one
// ascending sweep finds all the entry reaches.
function satisfiable(b, entry) {
  const n = b.kinds.length;
  const reach = new Uint8Array(n);
  reach[entry] = 1;
  for (let s = entry; s < n; s++) {
    if (!reach[s]) continue;
    const k = b.kinds[s];
    if (k === S_MATCH) return true;
    if (b.splitBs[s] !== UNSET) reach[b.splitBs[s]] = 1;
    if (k !== S_CLASS || !setEmpty(b.clsRefs[s])) reach[b.nexts[s]] = 1;
  }
  return false;
}

// Close an int32 `set` under ε-edges, as `closeWide` (compile only).
function closeSmall(nfa, set, block) {
  const one = Int32Array.of(set);
  closeWide(nfa, one, block);
  return one[0];
}

// Close `set` under ε-edges, keeping only consumers and S_MATCH. Every
// ε-edge points to a later state (see `compileNode`), so one ascending
// sweep sees each state after all that reach it; the word being swept
// stays in a local, where most edges land. Under `block` the consumers
// that refuse a protected leading `.` (`*`, `?`, negated classes) are
// dropped, and no `*` is passed.
function closeWide(nfa, set, block) {
  const kinds = nfa.kinds;
  const nexts = nfa.nexts;
  const splitBs = nfa.splitBs;
  for (let w = 0; w < set.length; w++) {
    let word = set[w];
    let todo = word;
    while (todo !== 0) {
      const bit = todo & -todo;
      todo ^= bit;
      const s = w * 32 + ctz32(bit);
      // The ε-targets `a`, `b` (-1: none).
      const k = kinds[s];
      let a = -1;
      let b = -1;
      if (k === S_SPLIT) {
        a = nexts[s];
        b = splitBs[s];
        word ^= bit;
      } else if (k === S_JUMP) {
        a = nexts[s];
        word ^= bit;
      } else if (k === S_ANY) {
        if (block) word ^= bit;
        else if (nexts[s] === s) a = splitBs[s];
      } else if (k === S_CLASS && block && nfa.clsRefs[s].neg) {
        word ^= bit;
      } else continue;
      if (a !== -1) {
        if (a >>> 5 === w) {
          word |= 1 << (a & 31);
          todo |= 1 << (a & 31);
        } else set[a >>> 5] |= 1 << (a & 31);
      }
      if (b !== -1) {
        if (b >>> 5 === w) {
          word |= 1 << (b & 31);
          todo |= 1 << (b & 31);
        } else set[b >>> 5] |= 1 << (b & 31);
      }
    }
    set[w] = word;
  }
}

// Up to 32 states each state's closure is kept, so a step ORs them.
class SegNfaSmall {
  constructor(b, entry, dot) {
    // Plain arrays: at this size a typed array costs more to make than
    // it saves.
    this.kinds = exact(b.kinds);
    this.byteVals = exact(b.byteVals);
    this.nexts = exact(b.nexts);
    this.clsRefs = exact(b.clsRefs);
    const n = b.kinds.length;

    // Closures, in one backward pass: every ε-edge points to a later
    // state, whose closure is done (a `*` stays in its own).
    const closures = [];
    for (let s = 0; s < n; s++) closures.push(0);
    for (let s = n - 1; s >= 0; s--) {
      const k = b.kinds[s];
      if (k === S_SPLIT) closures[s] = closures[b.nexts[s]] | closures[b.splitBs[s]];
      else if (k === S_JUMP) closures[s] = closures[b.nexts[s]];
      else if (k === S_ANY && b.nexts[s] === s) closures[s] = (1 << s) | closures[b.splitBs[s]];
      else closures[s] = 1 << s;
    }
    this.closures = exact(closures);
    this.init = closures[entry];
    // Under `dot` a leading `.` needs no protection, so the blocked
    // closure is just `init`.
    this.initDotBlocked = dot ? this.init : closeSmall(b, 1 << entry, true);
    this.acceptMask = 1 << (n - 1);
    this.countsBytes = b.countsBytes;
  }

  matches(str, s0, e0) {
    // Under dot=true the blocked closure equals `init`, so the compile
    // already collapsed the dot decision into which set this picks.
    const protectedStart = e0 > s0 && str.charCodeAt(s0) === 0x2e;
    let active = protectedStart ? this.initDotBlocked : this.init;
    const kinds = this.kinds;
    const byteVals = this.byteVals;
    const nexts = this.nexts;
    const clsRefs = this.clsRefs;
    const closures = this.closures;
    for (let i = s0; i < e0; i++) {
      if (active === 0) return false;
      const c = str.charCodeAt(i);
      let next = 0;
      let bits = active;
      while (bits !== 0) {
        const s = ctz32(bits);
        bits &= bits - 1;
        const k = kinds[s];
        if (k === S_BYTE) {
          if (byteVals[s] === c) next |= closures[nexts[s]];
        } else if (k === S_CLASS) {
          if (setHas(clsRefs[s], c)) next |= closures[nexts[s]];
        } else if (k === S_ANY) {
          next |= closures[nexts[s]];
        }
      }
      active = next;
    }
    return (active & this.acceptMask) !== 0;
  }
}

// Do the wildcards alone take every non-empty segment (`matchDir`'s
// all-below, see `coversBelow` in graph-nfa.js)?
function anySeg(b, entry, init, test) {
  const kinds = b.kinds;
  const n = kinds.length;
  // Most segments start with a literal or a class: none.
  let any = false;
  for (let s = 0; s < n && !any; s++) any = kinds[s] === S_ANY && test(init, s);
  if (!any) return false;
  const scratch = n <= LENS_SCRATCH;
  const lo = scratch ? LENS_LO.fill(0, 0, n) : new Int32Array(n);
  const hi = scratch ? LENS_HI.fill(0, 0, n) : new Int32Array(n);
  const tail = (scratch ? LENS_TAIL : new Float64Array(n)).fill(Infinity, 0, n);
  lo[entry] = 1;
  wildLengths(b, lo, hi, tail);
  return allNonEmpty(lo[n - 1], hi[n - 1], tail[n - 1]);
}

// The lengths of the segments the wildcards alone take from the states
// `lo` holds length 0 for to each state, in one pass in state order, as
// the Rust twin's `wild_lengths`: every edge but a star's loop onto
// itself points to a later state, so a state's are all in when it is
// reached. Lengths below 64 are one bit each, in `lo` (0-31) and `hi`
// (32-63), and every length from `tail` on is one too (Infinity: none
// known). Lengths past 63 below the tail are left out, which keeps out
// lengths only: a run of 70 `?` beside a `*` is not told to take every
// length.
function wildLengths(nfa, lo, hi, tail) {
  const kinds = nfa.kinds;
  const nexts = nfa.nexts;
  const splitBs = nfa.splitBs;
  for (let s = 0; s < kinds.length; s++) {
    const l = lo[s];
    const h = hi[s];
    let f = tail[s];
    if (l === 0 && h === 0 && f === Infinity) continue;
    const k = kinds[s];
    if (k === S_SPLIT) {
      addLens(lo, hi, tail, nexts[s], l, h, f);
      addLens(lo, hi, tail, splitBs[s], l, h, f);
    } else if (k === S_JUMP) {
      addLens(lo, hi, tail, nexts[s], l, h, f);
    } else if (k === S_ANY && nexts[s] === s) {
      // A `*` stays, then leaves: every length from the least on.
      const least = l !== 0 ? ctz32(l) : h !== 0 ? 32 + ctz32(h) : f;
      if (least < f) f = tail[s] = least;
      addLens(lo, hi, tail, splitBs[s], l, h, f);
    } else if (k === S_ANY) {
      // One wildcard more.
      addLens(lo, hi, tail, nexts[s], l << 1, (h << 1) | (l >>> 31), f + 1);
    }
  }
}

// State `t` is reached at the lengths `l`, `h`, `f` too.
function addLens(lo, hi, tail, t, l, h, f) {
  if (t === UNSET) return;
  lo[t] |= l;
  hi[t] |= h;
  if (f < tail[t]) tail[t] = f;
}

// The lengths of up to 64 states, one set for all builds: a typed
// array costs more to make than such a pass.
const LENS_SCRATCH = 64;
const LENS_LO = new Int32Array(LENS_SCRATCH);
const LENS_HI = new Int32Array(LENS_SCRATCH);
const LENS_TAIL = new Float64Array(LENS_SCRATCH);

// Every non-empty length: 1 to 63 one by one, and every longer one
// through the tail.
function allNonEmpty(lo, hi, tail) {
  if (tail < 32) {
    lo |= -1 << tail;
    hi = -1;
  } else if (tail < 64) hi |= -1 << (tail - 32);
  return tail <= 64 && (lo | 1) === -1 && hi === -1;
}

// The same automaton over Int32Array words: more than 32 states. The
// closures are one flat table, `words` words per state.
class SegNfaWide {
  constructor(b, entry, dot) {
    this.kinds = Uint8Array.from(b.kinds);
    this.byteVals = Uint8Array.from(b.byteVals);
    this.nexts = Int32Array.from(b.nexts);
    this.splitBs = Int32Array.from(b.splitBs);
    this.clsRefs = b.clsRefs.slice();
    const n = b.kinds.length;
    const words = wordsFor(n);
    this.words = words;
    this.closures = wideClosures(b, words);
    const entrySet = (block) => {
      const set = new Int32Array(words);
      bitSet(set, entry);
      closeWide(this, set, block);
      return set;
    };
    this.init = entrySet(false);
    this.initDotBlocked = dot ? this.init : entrySet(true);
    this.accept = n - 1;
    this.countsBytes = b.countsBytes;
  }

  matches(str, s0, e0) {
    const protectedStart = e0 > s0 && str.charCodeAt(s0) === 0x2e;
    const active = (protectedStart ? this.initDotBlocked : this.init).slice();
    return bitTest(runWide(this, active, str, s0, e0), this.accept);
  }
}

// `active` after every code unit of `str[s0..e0]`, or empty once nothing
// is left: through the kept closures, or without them (`closures` null)
// the consumers move and closeWide adds what their targets reach. It
// steps `active` in place.
function runWide(nfa, active, str, s0, e0) {
  let next = new Int32Array(active.length);
  for (let i = s0; i < e0; i++) {
    if (bitsIsZero(active)) break;
    const c = str.charCodeAt(i);
    if (nfa.closures === null) stepClosing(nfa, active, next, c);
    else stepKept(nfa, active, next, c);
    const t = active;
    active = next;
    next = t;
  }
  return active;
}

// `next` = what `active` reaches over code unit `c`, without kept
// closures: the consumers move, then closeWide adds what their targets
// reach.
function stepClosing(nfa, active, next, c) {
  next.fill(0);
  const kinds = nfa.kinds;
  for (let w = 0; w < active.length; w++) {
    let bits = active[w];
    while (bits !== 0) {
      const s = w * 32 + ctz32(bits);
      bits &= bits - 1;
      const k = kinds[s];
      if (
        k === S_ANY ||
        (k === S_BYTE && nfa.byteVals[s] === c) ||
        (k === S_CLASS && setHas(nfa.clsRefs[s], c))
      ) {
        bitSet(next, nfa.nexts[s]);
      }
    }
  }
  closeWide(nfa, next, false);
}

// `next` = what `active` reaches over code unit `c`, through the kept
// closures.
function stepKept(nfa, active, next, c) {
  next.fill(0);
  const words = active.length;
  const kinds = nfa.kinds;
  for (let w = 0; w < words; w++) {
    let bits = active[w];
    while (bits !== 0) {
      const s = w * 32 + ctz32(bits);
      bits &= bits - 1;
      const k = kinds[s];
      if (
        k === S_BYTE
          ? nfa.byteVals[s] === c
          : k === S_CLASS
            ? setHas(nfa.clsRefs[s], c)
            : k === S_ANY
      ) {
        const base = nfa.nexts[s] * words;
        for (let q = 0; q < words; q++) next[q] |= nfa.closures[base + q];
      }
    }
  }
}

// Per-state closures, `words` words each, within MAX_CLOSURE_BYTES, else
// none (null). One backward pass: every ε-edge points to a later state,
// whose closure is done (a `*` stays in its own).
function wideClosures(b, words) {
  const n = b.kinds.length;
  if (n * words * 4 > MAX_CLOSURE_BYTES) return null;
  const closures = new Int32Array(n * words);
  const copy = (s, from) => {
    for (let k = 0; k < words; k++) closures[s * words + k] |= closures[from * words + k];
  };
  for (let s = n - 1; s >= 0; s--) {
    const k = b.kinds[s];
    if (k === S_SPLIT) {
      copy(s, b.nexts[s]);
      copy(s, b.splitBs[s]);
    } else if (k === S_JUMP) {
      copy(s, b.nexts[s]);
    } else {
      if (k === S_ANY && b.nexts[s] === s) copy(s, b.splitBs[s]);
      closures[s * words + (s >>> 5)] |= 1 << (s & 31);
    }
  }
  return closures;
}

// The pieces of every segment a separator-crossing brace cuts (see
// graph.js), as one NFA of the same states. A piece is its nodes'
// states, then a fan of ε-edges: one to each piece glued after it, and an
// S_EXIT when its segment may end there. A join among pieces is the fan
// alone. A segment is matched from the entry states of the pieces the
// graph NFA holds, all at once, and the exits it reaches are where the
// graph NFA goes on. Nodes are laid out in graph order, and a piece is
// glued only to later ones, so every ε-edge still points to a later
// state. JS twin of `GlueNfa` in seg_nfa.rs.
export class GlueNfa {
  // The NFA of the nodes that are glued to their predecessors (`glued`)
  // or have a successor glued to them, each of which becomes an
  // EL_PIECE. `pieces` holds a wildcard node's nodes.
  constructor(elems, succs, glued, pieces, ci) {
    const n = elems.length;
    const gluedTo = (t) => t !== ACCEPT && glued[t] === 1;
    const b = new SegBuilder(ci);
    // Per graph node, the range of its states (empty outside the pieces).
    const spanLo = new Int32Array(n);
    const spanHi = new Int32Array(n);
    // The fan states that wait for a later piece's first state.
    const waiting = [];
    for (let i = 0; i < n; i++) {
      const succ = succs[i];
      if (glued[i] === 0 && !succ.some(gluedTo)) continue;
      const first = b.kinds.length;
      const e = elems[i];
      let nodes;
      if (e.kind === EL_LIT) nodes = [lit(e.litStr)];
      else if (e.kind === EL_WILD) nodes = pieces[i];
      // A join is the fan alone.
      else if (e.kind === EL_JOIN) nodes = [];
      else throw new Error("a globstar is never glued");
      const entry = b.compileNodes(nodes);
      for (const s of waiting[i] ?? []) b.patch(s, entry);
      const tails = b.tails;
      b.tails = [];
      const exit = succ.some((t) => !gluedTo(t));
      const glues = succ.filter(gluedTo);
      const ways = (exit ? 1 : 0) + glues.length;
      // Split `k` takes leaf `k` or the rest, as a brace's do.
      const fan = b.kinds.length;
      for (let k = 1; k < ways; k++) b.alloc(S_SPLIT, 0, UNSET);
      const leaves = [];
      if (exit) leaves.push(b.alloc(S_EXIT, 0, i));
      for (const t of glues) {
        const s = b.alloc(S_JUMP, 0, UNSET);
        (waiting[t] ??= []).push(s);
        leaves.push(s);
      }
      for (let k = 0; k + 1 < ways; k++) {
        b.nexts[fan + k] = leaves[k];
        b.splitBs[fan + k] = k + 2 < ways ? fan + k + 1 : leaves[k + 1];
      }
      const fanEntry = ways > 1 ? fan : leaves[0];
      for (const t of tails) b.patch(t, fanEntry);
      spanLo[i] = first;
      spanHi[i] = b.kinds.length;
      elems[i] = { kind: EL_PIECE, litStr: null, wild: null, entry: glued[i] ? INNER : entry };
    }
    this.kinds = Uint8Array.from(b.kinds);
    this.byteVals = Uint8Array.from(b.byteVals);
    this.nexts = Int32Array.from(b.nexts);
    this.splitBs = Int32Array.from(b.splitBs);
    this.clsRefs = b.clsRefs.slice();
    this.words = wordsFor(b.kinds.length);
    this.closures = wideClosures(b, this.words);
    this.countsBytes = b.countsBytes;
    this.spanLo = spanLo;
    this.spanHi = spanHi;
    // The lengths `anyExits` finds, one set for all its calls.
    this.lengths = null;
  }

  // An empty set of its states.
  set() {
    return new Int32Array(this.words);
  }

  // Calls `exit` with each node whose piece ends a match of `str[s0..e0]`
  // begun at the entry states in `active`, which it takes. A protected
  // leading `.` (`dot` off) is taken only by name, as in the segment
  // NFAs, and a non-ASCII segment runs on its UTF-8 bytes when a state
  // counts them (not `byteOnly`, see `wildConsumes`).
  exits(active, str, s0, e0, dot, byteOnly, exit) {
    if (this.countsBytes && !byteOnly && !isAscii(str, s0, e0)) {
      str = utf8Latin1(str.slice(s0, e0));
      s0 = 0;
      e0 = str.length;
    }
    closeWide(this, active, !dot && e0 > s0 && str.charCodeAt(s0) === 0x2e);
    active = runWide(this, active, str, s0, e0);
    for (let w = 0; w < active.length; w++) {
      for (let bits = active[w]; bits !== 0; bits &= bits - 1) {
        const s = w * 32 + ctz32(bits);
        if (this.kinds[s] === S_EXIT) exit(this.nexts[s]);
      }
    }
  }

  // Calls `exit` with each node a piece of which the wildcards alone,
  // from the entry states in `entries`, end every non-empty segment at
  // (dot=true), as `anySeg` reads a whole segment. A segment of a length
  // reaches the nodes it does for every segment of it, so a node stands
  // for all the segments between these entries and it, as a whole
  // segment does.
  anyExits(entries, exit) {
    const n = this.kinds.length;
    this.lengths ??= { lo: new Int32Array(n), hi: new Int32Array(n), tail: new Float64Array(n) };
    const { lo, hi, tail } = this.lengths;
    lo.fill(0);
    hi.fill(0);
    tail.fill(Infinity);
    for (let w = 0; w < entries.length; w++) {
      for (let bits = entries[w]; bits !== 0; bits &= bits - 1) lo[w * 32 + ctz32(bits)] |= 1;
    }
    wildLengths(this, lo, hi, tail);
    for (let s = 0; s < n; s++) {
      if (this.kinds[s] === S_EXIT && allNonEmpty(lo[s], hi[s], tail[s])) exit(this.nexts[s]);
    }
  }

  // The states of `node` from which some segment reaches an exit that
  // `good` holds for (`node`'s own when `exitOn`), added to `good`;
  // whether its entry is one. Called for each node from the last back:
  // every edge leads to a later state, so what a state leads to is known
  // when it is reached.
  reach(node, exitOn, good) {
    const lo = this.spanLo[node];
    const hi = this.spanHi[node];
    const to = (s, t) => t !== UNSET && t !== s && bitTest(good, t);
    for (let s = hi - 1; s >= lo; s--) {
      let on;
      switch (this.kinds[s]) {
        case S_EXIT:
          on = exitOn;
          break;
        case S_CLASS:
          on = !setEmpty(this.clsRefs[s]) && to(s, this.nexts[s]);
          break;
        case S_BYTE:
        case S_JUMP:
          on = to(s, this.nexts[s]);
          break;
        case S_ANY:
        case S_SPLIT:
          on = to(s, this.nexts[s]) || to(s, this.splitBs[s]);
          break;
        default:
          on = false;
      }
      if (on) bitSet(good, s);
    }
    return lo < hi && bitTest(good, lo);
  }
}

class SegBuilder {
  constructor(ci) {
    this.kinds = [];
    this.byteVals = [];
    this.nexts = [];
    this.splitBs = [];
    this.clsRefs = [];
    this.tails = [];
    this.ci = ci;
    // Only constructs that count (`?`, negated classes) see a
    // difference between a path string's UTF-16 units and its UTF-8
    // bytes, so only then a non-ASCII segment runs on its bytes. Star
    // bodies consume unbounded runs, so they never count.
    this.countsBytes = false;
  }

  alloc(kind, byteVal, next, splitB = UNSET, cls = null) {
    this.kinds.push(kind);
    this.byteVals.push(byteVal);
    this.nexts.push(next);
    this.splitBs.push(splitB);
    this.clsRefs.push(cls);
    return this.kinds.length - 1;
  }

  // Point the dangling edge of `state` at `target`: a `*`'s way out, or
  // the `next` of anything else.
  patch(state, target) {
    // A Split is never a dangling tail: a brace returns its branches'
    // leaf tails.
    if (this.kinds[state] === S_SPLIT) throw new Error("patch: unreachable Split");
    if (this.kinds[state] === S_ANY && this.nexts[state] === state) this.splitBs[state] = target;
    else this.nexts[state] = target;
  }

  compileNodes(nodes) {
    if (nodes.length === 0) {
      const s = this.alloc(S_JUMP, 0, UNSET);
      this.tails.push(s);
      return s;
    }
    let entry = -1;
    let pending = [];
    for (const node of nodes) {
      const [nodeEntry, nodeTails] = this.compileNode(node);
      for (const t of pending) this.patch(t, nodeEntry);
      pending = nodeTails;
      if (entry === -1) entry = nodeEntry;
    }
    for (const t of pending) this.tails.push(t);
    return entry;
  }

  compileNode(node) {
    switch (node.tag) {
      case N_LITERAL: {
        const str = node.str;
        const entry = this.litState(str.charCodeAt(0));
        let prev = entry;
        for (let i = 1; i < str.length; i++) {
          const s = this.litState(str.charCodeAt(i));
          this.patch(prev, s);
          prev = s;
        }
        return [entry, [prev]];
      }
      case N_ANYCHAR: {
        this.countsBytes = true;
        const s = this.alloc(S_ANY, 0, UNSET);
        return [s, [s]];
      }
      case N_CLASS: {
        if (node.neg) this.countsBytes = true;
        const s = this.alloc(S_CLASS, 0, UNSET, UNSET, byteSet(node, this.ci));
        return [s, [s]];
      }
      case N_STAR: {
        // Takes any unit and stays; its way out is patched into `splitBs`.
        const s = this.alloc(S_ANY, 0, this.kinds.length);
        return [s, [s]];
      }
      case N_BRACE: {
        // The splits come before the branches, so every ε-edge points to
        // a later state (the closure passes rely on it).
        const first = this.kinds.length;
        for (let i = 1; i < node.branches.length; i++) this.alloc(S_SPLIT, 0, UNSET);
        // `this.tails` is empty here (every caller drains it), so the
        // branch tails can be collected straight out of it.
        const entries = [];
        const tails = [];
        for (const branch of node.branches) {
          entries.push(this.compileNodes(seq(branch)));
          for (const t of this.tails) tails.push(t);
          this.tails = [];
        }
        // Split `i` picks branch `i` or the rest: the next split, or the
        // last branch.
        const last = entries.length - 1;
        for (let i = 0; i < last; i++) {
          this.nexts[first + i] = entries[i];
          this.splitBs[first + i] = i + 1 < last ? first + i + 1 : entries[last];
        }
        return [last === 0 ? entries[0] : first, tails];
      }
      default:
        throw new Error("separator-crossing node inside a segment");
    }
  }

  litState(b) {
    const cls = this.ci ? letterClass(b) : null;
    if (cls !== null) return this.alloc(S_CLASS, 0, UNSET, UNSET, cls);
    return this.alloc(S_BYTE, b, UNSET);
  }
}
