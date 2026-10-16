// In-segment mini NFA — the `WK_GENERIC` wild backend. JS port of the
// Rust module `crates/globstar/src/engine/segment/seg_nfa.rs`: a
// Thompson NFA over the in-segment ops, simulated with an int32 bit set
// for up to 32 states and Int32Array words beyond, so no in-segment
// pattern is too large.

import { OP_LIT, OP_ANYCHAR, OP_STAR, OP_CLASS, OP_ALTERNATION } from "../ops.js";
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

// Per-state "some byte fires this consumer", computed once (a 256-scan
// per class state inside the fixpoint was the dominant compile cost
// for class patterns).
function fireNext(b) {
  const n = b.kinds.length;
  const out = [];
  for (let s = 0; s < n; s++) out.push(-1);
  for (let s = 0; s < n; s++) {
    const k = b.kinds[s];
    if (k === S_BYTE || k === S_ANY) out[s] = b.nexts[s];
    else if (k === S_CLASS) {
      const cls = b.clsRefs[s];
      for (let c = 0; c <= 255; c++) {
        if (classMatches(cls, c)) {
          out[s] = b.nexts[s];
          break;
        }
      }
    }
  }
  return out;
}

class SegNfaSmall {
  constructor(b, entry, dot) {
    this.kinds = b.kinds;
    this.byteVals = b.byteVals;
    this.nexts = b.nexts;
    this.clsRefs = b.clsRefs;
    const n = b.kinds.length;

    // Guard-passing closures. Every ε-edge points to a later state (see
    // `compileOp`), so one backward pass folds each closure from its
    // children's.
    const closures = [];
    for (let s = 0; s < n; s++) closures.push(0);
    for (let s = n - 1; s >= 0; s--) {
      const k = b.kinds[s];
      if (k === S_SPLIT) closures[s] = closures[b.nexts[s]] | closures[b.splitBs[s]];
      else if (k === S_JUMP || k === S_DOT_GUARD) closures[s] = closures[b.nexts[s]];
      else closures[s] = 1 << s;
    }
    this.closures = closures;
    this.init = closures[entry];
    // Under `dot` a leading `.` needs no protection, so the blocked
    // closure is just `init` (DotGuards behave like Jumps).
    this.initDotBlocked = dot ? this.init : blockedSmall(b, entry);
    this.acceptMask = 1 << (n - 1);
    this.wildLed = wildLed(b, this.initDotBlocked, (a, s) => (a & (1 << s)) !== 0);
    this.needsAsciiSeg = b.needsAsciiSeg;
    this.satisfiable = satisfiableSmall(this, fireNext(b));
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

// Does a state stay in the entry set of a protected leading `.`? A cut
// S_DOT_GUARD and the consumers that refuse that `.` (S_ANY, negated
// classes) do not; Split/Jump are not leaves.
function blockedLeaf(b, s) {
  const k = b.kinds[s];
  return k === S_BYTE || k === S_MATCH || (k === S_CLASS && !b.clsRefs[s].neg);
}

// The entry set for a protected leading `.`. ε-edges point forward, so
// one sweep from the entry finds every state it reaches.
function blockedSmall(b, entry) {
  let reach = 1 << entry;
  let out = 0;
  for (let s = entry; s < b.kinds.length; s++) {
    if ((reach & (1 << s)) === 0) continue;
    const k = b.kinds[s];
    if (k === S_SPLIT) reach |= (1 << b.nexts[s]) | (1 << b.splitBs[s]);
    else if (k === S_JUMP) reach |= 1 << b.nexts[s];
    else if (blockedLeaf(b, s)) out |= 1 << s;
  }
  return out;
}

function satisfiableSmall(nfa, fire) {
  let reach = nfa.init;
  let work = 0;
  for (let s = 0; s < fire.length; s++) if (fire[s] !== -1) work |= 1 << s;
  work &= reach;
  while (work !== 0) {
    const s = ctz32(work);
    work &= work - 1;
    const grown = nfa.closures[fire[s]] & ~reach;
    if (grown !== 0) {
      reach |= grown;
      let g = grown;
      while (g !== 0) {
        const t = ctz32(g);
        g &= g - 1;
        if (fire[t] !== -1) work |= 1 << t;
      }
    }
  }
  return (reach & nfa.acceptMask) !== 0;
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
    this.kinds = b.kinds;
    this.byteVals = b.byteVals;
    this.nexts = b.nexts;
    this.clsRefs = b.clsRefs;
    const n = b.kinds.length;
    const words = wordsFor(n);
    this.words = words;
    const closures = [];
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
    this.closures = closures;
    this.init = closures[entry];
    this.initDotBlocked = dot ? this.init : blockedWide(b, entry, words);
    this.accept = n - 1;
    this.wildLed = wildLed(b, this.initDotBlocked, bitTest);
    this.needsAsciiSeg = b.needsAsciiSeg;
    this.satisfiable = satisfiableWide(this, fireNext(b));
    this.anySeg = anySegWide(this);
  }

  matches(str, s0, e0) {
    const protectedStart = e0 > s0 && str.charCodeAt(s0) === 0x2e;
    let active = (protectedStart ? this.initDotBlocked : this.init).slice();
    let next = new Int32Array(this.words);
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

function blockedWide(b, entry, words) {
  const reach = new Int32Array(words);
  const out = new Int32Array(words);
  bitSet(reach, entry);
  for (let s = entry; s < b.kinds.length; s++) {
    if (!bitTest(reach, s)) continue;
    const k = b.kinds[s];
    if (k === S_SPLIT) {
      bitSet(reach, b.nexts[s]);
      bitSet(reach, b.splitBs[s]);
    } else if (k === S_JUMP) bitSet(reach, b.nexts[s]);
    else if (blockedLeaf(b, s)) bitSet(out, s);
  }
  return out;
}

function satisfiableWide(nfa, fire) {
  const reach = nfa.init.slice();
  const work = [];
  bitsForEach(nfa.init, (s) => {
    if (fire[s] !== -1) work.push(s);
  });
  while (work.length > 0) {
    const s = work.pop();
    bitsForEach(nfa.closures[fire[s]], (t) => {
      if (!bitTest(reach, t)) {
        bitSet(reach, t);
        if (fire[t] !== -1) work.push(t);
      }
    });
  }
  return bitTest(reach, nfa.accept);
}

function anySegWide(nfa) {
  const kinds = nfa.kinds;
  let active = nfa.init.slice();
  let next = new Int32Array(nfa.words);
  for (let step = 0; step <= kinds.length; step++) {
    next.fill(0);
    bitsForEach(active, (s) => {
      if (kinds[s] === S_ANY) bitsOr(next, nfa.closures[nfa.nexts[s]]);
    });
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
