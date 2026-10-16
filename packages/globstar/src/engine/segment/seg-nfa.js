// In-segment mini NFA — the `WK_GENERIC` wild backend. JS port of the
// Rust module `crates/globstar/src/engine/segment/seg_nfa.rs`: a
// Thompson NFA over the in-segment ops, simulated with an int32 bit set
// for up to 32 states and Int32Array words beyond, so no in-segment
// pattern is too large.
//
// Every ε-edge points to a later state (see `compileOp`), so one
// ascending sweep closes a set. A step ORs per-state closures while they
// fit MAX_CLOSURE_BYTES; past that (a dense set per state is quadratic in
// the states) it closes its set as it goes, so memory stays linear in
// the pattern.

import { OP_LIT, OP_ANYCHAR, OP_STAR, OP_CLASS, OP_ALTERNATION, exact } from "../ops.js";
import { classMatches, ciLetter } from "../../ast.js";
import { ctz32 } from "../../bytes.js";
import { wordsFor, bitTest, bitSet, bitsOr, bitsIsZero, bitsEqual, bitsForEach } from "./bits.js";

const S_BYTE = 0;
const S_CLASS = 1;
const S_ANY = 2;
const S_SPLIT = 3;
const S_JUMP = 4;
const S_DOT_GUARD = 5;
const S_MATCH = 6;

const UNSET = -1;

// The most the per-state closures may take, about 1,000 states. Up to there
// ORing dense closures is the faster step; past it, closing the set as
// the step goes wins and keeps memory linear. (Rust's ORs are faster, so
// its budget is bigger.)
const MAX_CLOSURE_BYTES = 128 * 1024;

export function compileSegNfa(ops, dot, ci) {
  const b = new SegBuilder(ci);
  const entry = b.compileOps(ops);
  const accept = b.alloc(S_MATCH, 0, UNSET);
  for (const t of b.tails) b.patch(t, accept);
  return b.kinds.length <= 32 ? new SegNfaSmall(b, entry, dot) : new SegNfaWide(b, entry, dot);
}

// Shared post-construction facts: a protected leading `.` can only be
// taken by a literal `.` or a positive class containing it.
function wildLed(b, initDotBlocked, test) {
  const n = b.kinds.length;
  for (let s = 0; s < n; s++) {
    if (!test(initDotBlocked, s)) continue;
    if (b.kinds[s] === S_BYTE && b.byteVals[s] === 0x2e) return false;
    if (b.kinds[s] === S_CLASS && classMatches(b.clsRefs[s], 0x2e)) return false;
  }
  return true;
}

// Can the entry reach S_MATCH through consumers some byte fires? Every
// edge but a star's loop back to its own split points to a later state,
// so one ascending sweep finds all the entry reaches.
function satisfiable(b, entry) {
  const n = b.kinds.length;
  const reach = [];
  for (let s = 0; s < n; s++) reach.push(s === entry);
  for (let s = entry; s < n; s++) {
    if (!reach[s]) continue;
    const k = b.kinds[s];
    if (k === S_MATCH) return true;
    if (k === S_SPLIT) reach[b.splitBs[s]] = true;
    if (k !== S_CLASS || classFires(b.clsRefs[s])) reach[b.nexts[s]] = true;
  }
  return false;
}

function classFires(cls) {
  for (let c = 0; c <= 255; c++) if (classMatches(cls, c)) return true;
  return false;
}

// What closing a set does with state `s`: -2 keeps it (a consumer or
// S_MATCH); otherwise it leaves the set for its first ε-successor, -1
// for none (a Split adds its second one too). Under `block` S_DOT_GUARD
// is cut, and S_ANY and negated classes refuse a protected leading `.`.
function epsA(nfa, s, block) {
  const k = nfa.kinds[s];
  if (k === S_SPLIT || k === S_JUMP) return nfa.nexts[s];
  if (k === S_DOT_GUARD) return block ? -1 : nfa.nexts[s];
  if (block && (k === S_ANY || (k === S_CLASS && nfa.clsRefs[s].neg))) return -1;
  return -2;
}

// Close `set` under ε-edges, keeping only consumers and S_MATCH. Every
// ε-edge points to a later state (see `compileOp`), so one ascending
// sweep sees each state after all that reach it; the word being swept
// stays in a local, where most edges land.
function closeSmall(nfa, set, block) {
  let todo = set;
  while (todo !== 0) {
    const bit = todo & -todo;
    todo ^= bit;
    const s = ctz32(bit);
    const a = epsA(nfa, s, block);
    if (a === -2) continue;
    set ^= bit;
    if (a !== -1) {
      set |= 1 << a;
      todo |= 1 << a;
    }
    if (nfa.kinds[s] === S_SPLIT) {
      set |= 1 << nfa.splitBs[s];
      todo |= 1 << nfa.splitBs[s];
    }
  }
  return set;
}

function closeWide(nfa, set, block) {
  for (let w = 0; w < set.length; w++) {
    let word = set[w];
    let todo = word;
    while (todo !== 0) {
      const bit = todo & -todo;
      todo ^= bit;
      const s = w * 32 + ctz32(bit);
      const a = epsA(nfa, s, block);
      if (a === -2) continue;
      word ^= bit;
      if (a !== -1) {
        if (a >>> 5 === w) {
          word |= 1 << (a & 31);
          todo |= 1 << (a & 31);
        } else set[a >>> 5] |= 1 << (a & 31);
      }
      if (nfa.kinds[s] === S_SPLIT) {
        const b = nfa.splitBs[s];
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
    this.kinds = exact(b.kinds);
    this.byteVals = exact(b.byteVals);
    this.nexts = exact(b.nexts);
    this.clsRefs = exact(b.clsRefs);
    const n = b.kinds.length;

    // Guard-passing closures, in one backward pass: every ε-edge points
    // to a later state, whose closure is done.
    const closures = [];
    for (let s = 0; s < n; s++) closures.push(0);
    for (let s = n - 1; s >= 0; s--) {
      const k = b.kinds[s];
      if (k === S_SPLIT) closures[s] = closures[b.nexts[s]] | closures[b.splitBs[s]];
      else if (k === S_JUMP || k === S_DOT_GUARD) closures[s] = closures[b.nexts[s]];
      else closures[s] = 1 << s;
    }
    this.closures = exact(closures);
    this.init = closures[entry];
    // Under `dot` a leading `.` needs no protection, so the blocked
    // closure is just `init` (DotGuards behave like Jumps).
    this.initDotBlocked = dot ? this.init : closeSmall(b, 1 << entry, true);
    this.acceptMask = 1 << (n - 1);
    this.wildLed = wildLed(b, this.initDotBlocked, (a, s) => (a & (1 << s)) !== 0);
    this.needsAsciiSeg = b.needsAsciiSeg;
    this.satisfiable = satisfiable(b, entry);
    // Does the NFA accept every non-empty segment through its S_ANY
    // states alone? (`matchDir` all-below.)
    this.anySeg = anySegSmall(this);
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
          if (classMatches(clsRefs[s], c)) next |= closures[nexts[s]];
        } else if (k === S_ANY) {
          next |= closures[nexts[s]];
        }
      }
      active = next;
    }
    return (active & this.acceptMask) !== 0;
  }
}

// Step the entry set through S_ANY states only, one byte at a time, until
// it settles; every length from 1 up must accept.
function anySegSmall(nfa) {
  const kinds = nfa.kinds;
  let active = nfa.init;
  // Every loop is a star's one-byte self-loop, so the set settles within
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

// The same automaton over Int32Array words: more than 32 states.
class SegNfaWide {
  constructor(b, entry, dot) {
    this.kinds = exact(b.kinds);
    this.byteVals = exact(b.byteVals);
    this.nexts = exact(b.nexts);
    this.splitBs = exact(b.splitBs);
    this.clsRefs = exact(b.clsRefs);
    const n = b.kinds.length;
    const words = wordsFor(n);
    this.words = words;
    // Per-state closures within MAX_CLOSURE_BYTES, else none (null).
    let closures = null;
    if (n * words * 4 <= MAX_CLOSURE_BYTES) {
      closures = [];
      for (let s = 0; s < n; s++) closures.push(null);
      for (let s = n - 1; s >= 0; s--) {
        const k = b.kinds[s];
        if (k === S_SPLIT) {
          const c = closures[b.nexts[s]].slice();
          bitsOr(c, closures[b.splitBs[s]]);
          closures[s] = c;
        } else if (k === S_JUMP || k === S_DOT_GUARD) {
          closures[s] = closures[b.nexts[s]];
        } else {
          const c = new Int32Array(words);
          bitSet(c, s);
          closures[s] = c;
        }
      }
    }
    this.closures = closures === null ? null : exact(closures);
    const entrySet = (block) => {
      const set = new Int32Array(this.words);
      bitSet(set, entry);
      closeWide(this, set, block);
      return set;
    };
    this.init = entrySet(false);
    this.initDotBlocked = dot ? this.init : entrySet(true);
    this.accept = n - 1;
    this.wildLed = wildLed(b, this.initDotBlocked, bitTest);
    this.needsAsciiSeg = b.needsAsciiSeg;
    this.satisfiable = satisfiable(b, entry);
    this.anySeg = anySegWide(this);
  }

  matches(str, s0, e0) {
    const protectedStart = e0 > s0 && str.charCodeAt(s0) === 0x2e;
    let active = (protectedStart ? this.initDotBlocked : this.init).slice();
    let next = new Int32Array(this.words);
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
      bitsForEach(active, (s) => {
        const k = kinds[s];
        if (k === S_BYTE) {
          if (byteVals[s] === c) bitsOr(next, closures[nexts[s]]);
        } else if (k === S_CLASS) {
          if (classMatches(clsRefs[s], c)) bitsOr(next, closures[nexts[s]]);
        } else if (k === S_ANY) {
          bitsOr(next, closures[nexts[s]]);
        }
      });
      const t = active;
      active = next;
      next = t;
    }
    return bitTest(active, this.accept);
  }
}

// `next` = what `active` reaches over code unit `c` (-1: one only S_ANY
// takes), without kept closures: the consumers move, then closeWide adds
// what their targets reach.
function stepClosing(nfa, active, next, c) {
  next.fill(0);
  bitsForEach(active, (s) => {
    const k = nfa.kinds[s];
    if (
      k === S_ANY ||
      (k === S_BYTE && nfa.byteVals[s] === c) ||
      (k === S_CLASS && c !== -1 && classMatches(nfa.clsRefs[s], c))
    ) {
      bitSet(next, nfa.nexts[s]);
    }
  });
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
    // String-mode bail requirement: only unit-COUNTING constructs (`?`,
    // negated classes) differ between UTF-16 and byte matching. Star
    // bodies consume unbounded runs, so they never count.
    this.needsAsciiSeg = false;
  }

  alloc(kind, byteVal, next, splitB = UNSET, cls = null) {
    this.kinds.push(kind);
    this.byteVals.push(byteVal);
    this.nexts.push(next);
    this.splitBs.push(splitB);
    this.clsRefs.push(cls);
    return this.kinds.length - 1;
  }

  patch(state, target) {
    // A Split is never a dangling tail: Star returns its exit state and
    // Alternation returns its branches' leaf tails.
    if (this.kinds[state] === S_SPLIT) throw new Error("patch: unreachable Split");
    this.nexts[state] = target;
  }

  compileOps(ops) {
    if (ops.length === 0) {
      const s = this.alloc(S_JUMP, 0, UNSET);
      this.tails.push(s);
      return s;
    }
    let entry = -1;
    let pending = [];
    for (const op of ops) {
      const [opEntry, opTails] = this.compileOp(op);
      for (const t of pending) this.patch(t, opEntry);
      pending = opTails;
      if (entry === -1) entry = opEntry;
    }
    for (const t of pending) this.tails.push(t);
    return entry;
  }

  compileOp(op) {
    switch (op.kind) {
      case OP_LIT: {
        const bytes = op.bytes;
        const entry = this.litState(bytes[0]);
        let prev = entry;
        for (let i = 1; i < bytes.length; i++) {
          const s = this.litState(bytes[i]);
          this.patch(prev, s);
          prev = s;
        }
        return [entry, [prev]];
      }
      case OP_ANYCHAR: {
        this.needsAsciiSeg = true;
        const s = this.alloc(S_ANY, 0, UNSET);
        return [s, [s]];
      }
      case OP_CLASS: {
        if (op.cls.neg) this.needsAsciiSeg = true;
        const s = this.alloc(S_CLASS, 0, UNSET, UNSET, op.cls);
        return [s, [s]];
      }
      case OP_STAR: {
        const entry = this.alloc(S_SPLIT, 0, UNSET);
        const body = this.alloc(S_ANY, 0, entry);
        // Under `dot` the guard is inert (blocked closures are never
        // built), so S_DOT_GUARD serves both compiles.
        const exit = this.alloc(S_DOT_GUARD, 0, UNSET);
        this.nexts[entry] = body;
        this.splitBs[entry] = exit;
        return [entry, [exit]];
      }
      case OP_ALTERNATION: {
        // The splits come before the branches, so every ε-edge points to
        // a later state (the closure passes rely on it).
        const first = this.kinds.length;
        for (let i = 1; i < op.branches.length; i++) this.alloc(S_SPLIT, 0, UNSET);
        // `this.tails` is empty here (every caller drains it), so the
        // branch tails can be collected straight out of it.
        const entries = [];
        const tails = [];
        for (const branch of op.branches) {
          entries.push(this.compileOps(branch));
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
        throw new Error("separator-crossing op inside a segment");
    }
  }

  litState(b) {
    if (this.ci) {
      const cls = ciLetter(b);
      if (cls !== null) return this.alloc(S_CLASS, 0, UNSET, UNSET, cls);
    }
    return this.alloc(S_BYTE, b, UNSET);
  }
}
