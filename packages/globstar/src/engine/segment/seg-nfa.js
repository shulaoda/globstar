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

import { N_ANYCHAR, N_BRACE, N_CLASS, N_LITERAL, N_STAR, seq } from "../../parse/ast.js";
import { ctz32, IS_WINDOWS_SEP } from "../../bytes.js";
import { exact, NEVER_LEN } from "./elem.js";
import { wordsFor, bitTest, bitSet, bitsIsZero, bitsEqual } from "./bits.js";

// NFA state kinds. A `*` is an S_ANY whose `next` is itself, with its
// way out in `splitBs`: the hot loop treats it like `?` (a separate kind
// cost 5-20% there).
const S_BYTE = 0;
const S_CLASS = 1;
const S_ANY = 2;
const S_SPLIT = 3;
const S_JUMP = 4;
const S_MATCH = 5;

const UNSET = -1;

// The most the per-state closures may take, about 1,000 states. Up to there
// ORing dense closures is the faster step; past it, closing the set as
// the step goes wins and keeps memory linear.
const MAX_CLOSURE_BYTES = 128 * 1024;

// The bytes `states` NFA states take in the Rust twin (16 each), with
// their closures while those are kept: what the compile walk charges
// for a segment's NFA (`tableBytes` in graph.js).
export function nfaTableBytes(states) {
  const closures = states * Math.ceil(states / 64) * 8;
  return states * 16 + (closures <= MAX_CLOSURE_BYTES ? closures : 0);
}

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
    nfa,
    minLen: satisfiable(b, entry) ? 0 : NEVER_LEN,
    dotProtect: !dot && wildLed(b, nfa.initDotBlocked, test),
    // Does it accept every non-empty segment through its wildcards
    // alone? (`matchDir` all-below.)
    anySeg: small ? anySegSmall(nfa) : anySegWide(nfa),
  };
}

// A class as the bytes it takes, one bit each in eight int32 words, with
// its negation applied and the separators left out: a test is one
// lookup, however many items the class lists. `neg` stays for dot
// protection, where a negated class counts as a wildcard.
// Case-insensitively (`ci`) a listed ASCII letter lists its other case
// too, before the negation (§13).
function byteSet(cls, ci) {
  const bits = [0, 0, 0, 0, 0, 0, 0, 0];
  // A word at a time: a class costs its ranges, not the bytes they span.
  for (const { lo, hi } of cls.ranges) {
    for (let w = lo >> 5; w <= hi >> 5; w++) {
      const from = w === lo >> 5 ? lo & 31 : 0;
      const to = w === hi >> 5 ? hi & 31 : 31;
      bits[w] |= (-1 >>> (31 - (to - from))) << from;
    }
  }
  const has = (b) => ((bits[b >> 5] >>> (b & 31)) & 1) === 1;
  for (let b = 0x41; b <= 0x5a; b++) {
    if (ci && (has(b) || has(b | 0x20))) {
      bits[b >> 5] |= 1 << (b & 31);
      bits[(b | 0x20) >> 5] |= 1 << ((b | 0x20) & 31);
    }
  }
  if (cls.neg) for (let w = 0; w < 8; w++) bits[w] = ~bits[w];
  bits[0x2f >> 5] &= ~(1 << (0x2f & 31));
  if (IS_WINDOWS_SEP) bits[0x5c >> 5] &= ~(1 << (0x5c & 31));
  return { neg: cls.neg, bits };
}

const setHas = (set, c) => ((set.bits[c >> 5] >>> (c & 31)) & 1) === 1;
const setEmpty = (set) => set.bits.every((w) => w === 0);

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

// Step the entry set through the wildcards only, one unit at a time,
// until it settles; every length from 1 up must accept.
function anySegSmall(nfa) {
  const kinds = nfa.kinds;
  let active = nfa.init;
  // Every loop is a star's one-unit self-loop, so the set settles within
  // the number of states.
  for (let step = 0; step <= kinds.length; step++) {
    let next = 0;
    let bits = active;
    while (bits !== 0) {
      const s = ctz32(bits);
      bits &= bits - 1;
      if (kinds[s] === S_ANY) next |= nfa.closures[nfa.nexts[s]];
    }
    if ((next & nfa.acceptMask) === 0) return false;
    if (next === active) return true;
    active = next;
  }
  return false;
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
    // Per-state closures within MAX_CLOSURE_BYTES, else none (null).
    let closures = null;
    if (n * words * 4 <= MAX_CLOSURE_BYTES) {
      closures = new Int32Array(n * words);
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
    }
    this.closures = closures;
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
    let active = (protectedStart ? this.initDotBlocked : this.init).slice();
    const words = this.words;
    let next = new Int32Array(words);
    if (this.closures === null) {
      for (let i = s0; i < e0; i++) {
        if (bitsIsZero(active)) return false;
        stepClosing(this, active, next, str.charCodeAt(i));
        const t = active;
        active = next;
        next = t;
      }
      return bitTest(active, this.accept);
    }
    const kinds = this.kinds;
    const byteVals = this.byteVals;
    const nexts = this.nexts;
    const clsRefs = this.clsRefs;
    const closures = this.closures;
    for (let i = s0; i < e0; i++) {
      if (bitsIsZero(active)) return false;
      const c = str.charCodeAt(i);
      next.fill(0);
      for (let w = 0; w < words; w++) {
        let bits = active[w];
        while (bits !== 0) {
          const s = w * 32 + ctz32(bits);
          bits &= bits - 1;
          const k = kinds[s];
          if (
            k === S_BYTE ? byteVals[s] === c : k === S_CLASS ? setHas(clsRefs[s], c) : k === S_ANY
          ) {
            const base = nexts[s] * words;
            for (let q = 0; q < words; q++) next[q] |= closures[base + q];
          }
        }
      }
      const t = active;
      active = next;
      next = t;
    }
    return bitTest(active, this.accept);
  }
}

// `next` = what `active` reaches over code unit `c` (-1: one only a
// wildcard takes), without kept closures: the consumers move, then
// closeWide adds what their targets reach.
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
        (k === S_CLASS && c !== -1 && setHas(nfa.clsRefs[s], c))
      ) {
        bitSet(next, nfa.nexts[s]);
      }
    }
  }
  closeWide(nfa, next, false);
}

function anySegWide(nfa) {
  const kinds = nfa.kinds;
  let active = nfa.init.slice();
  let next = new Int32Array(nfa.words);
  for (let step = 0; step <= kinds.length; step++) {
    stepClosing(nfa, active, next, -1);
    if (!bitTest(next, nfa.accept)) return false;
    if (bitsEqual(next, active)) return true;
    const t = active;
    active = next;
    next = t;
  }
  return false;
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
