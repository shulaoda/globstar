// In-segment mini NFA — the `WK_GENERIC` wild backend. JS port of the
// Rust module `crates/globstar/src/engine/segment/seg_nfa.rs`.

import { OP_LIT, OP_ANYCHAR, OP_STAR, OP_CLASS, OP_ALTERNATION } from "../ops/index.js";
import { classMatches, ciLetter } from "../../ast.js";
import { ctz32 } from "../../options.js";

const MAX_SEG_NFA_STATES = 32;

const S_BYTE = 0;
const S_CLASS = 1;
const S_ANY = 2;
const S_SPLIT = 3;
const S_JUMP = 4;
const S_DOT_GUARD = 5;
const S_MATCH = 6;

const UNSET = 0xff;

export class SegNfa {
  constructor(kinds, byteVals, nexts, splitBs, clsRefs, entry, dot, needsAsciiSeg) {
    this.kinds = kinds;
    this.byteVals = byteVals;
    this.nexts = nexts;
    this.clsRefs = clsRefs;
    this.dot = dot;
    const n = kinds.length;

    // Memoized guard-passing closures: the ε-graph (Split/Jump/Guard
    // edges) is acyclic — every pattern loop passes through a
    // consumer — so each state's closure folds from its children
    // exactly once.
    const closures = new Array(n).fill(-1);
    for (let s = 0; s < n; s++) memoClosure(kinds, nexts, splitBs, clsRefs, closures, s, false);
    this.closures = closures;
    this.init = closures[entry];
    // Under `dot` no S_DOT_GUARD state exists (the only alloc site is
    // gated on `!dot`), so the blocked closure is just `init`.
    this.initDotBlocked = dot
      ? this.init
      : memoClosure(kinds, nexts, splitBs, clsRefs, new Array(n).fill(-1), entry, true);
    this.acceptMask = 1 << (n - 1);

    // wildLed: no state of the protected entry set can consume a
    // leading `.`.
    let canLitDot = false;
    let bits = this.initDotBlocked;
    while (bits !== 0) {
      const s = ctz32(bits);
      bits &= bits - 1;
      if (kinds[s] === S_BYTE && byteVals[s] === 0x2e) canLitDot = true;
      else if (kinds[s] === S_CLASS && classMatches(clsRefs[s], 0x2e)) canLitDot = true;
    }
    this.wildLed = !canLitDot;

    this.needsAsciiSeg = needsAsciiSeg;

    this.satisfiable = computeSatisfiable(this);
  }

  static compile(ops, dot, ci) {
    const b = new SegBuilder(ci);
    const entry = b.compileOps(ops, dot);
    if (entry === -1) return null;
    const accept = b.alloc(S_MATCH, 0, UNSET);
    if (accept === -1) return null;
    for (const t of b.tails) b.patch(t, accept);
    return new SegNfa(
      b.kinds,
      b.byteVals,
      b.nexts,
      b.splitBs,
      b.clsRefs,
      entry,
      dot,
      b.needsAsciiSeg,
    );
  }

  matchesStr(str, s0, e0) {
    const protectedStart = !this.dot && e0 > s0 && str.charCodeAt(s0) === 0x2e;
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

  matchesBytes(bytes, s0, e0) {
    const protectedStart = !this.dot && e0 > s0 && bytes[s0] === 0x2e;
    let active = protectedStart ? this.initDotBlocked : this.init;
    const kinds = this.kinds;
    const byteVals = this.byteVals;
    const nexts = this.nexts;
    const clsRefs = this.clsRefs;
    const closures = this.closures;
    for (let i = s0; i < e0; i++) {
      if (active === 0) return false;
      const c = bytes[i];
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

// Memoized ε-closure. `block` builds the entry set for a protected
// leading `.`, cutting S_DOT_GUARD edges and dropping the consumers
// that refuse that `.` (S_ANY, negated classes); otherwise guards pass
// and every consumer is a leaf. `memo[s] === -1` means uncomputed; the
// ε-graph is acyclic so plain recursion terminates.
function memoClosure(kinds, nexts, splitBs, clsRefs, memo, s, block) {
  const cached = memo[s];
  if (cached !== -1) return cached;
  const k = kinds[s];
  let out;
  if (k === S_SPLIT) {
    out =
      memoClosure(kinds, nexts, splitBs, clsRefs, memo, nexts[s], block) |
      memoClosure(kinds, nexts, splitBs, clsRefs, memo, splitBs[s], block);
  } else if (k === S_JUMP) {
    out = memoClosure(kinds, nexts, splitBs, clsRefs, memo, nexts[s], block);
  } else if (k === S_DOT_GUARD) {
    out = block ? 0 : memoClosure(kinds, nexts, splitBs, clsRefs, memo, nexts[s], block);
  } else if (block && (k === S_ANY || (k === S_CLASS && clsRefs[s].neg))) {
    out = 0;
  } else {
    out = 1 << s;
  }
  memo[s] = out;
  return out;
}

function computeSatisfiable(nfa) {
  const kinds = nfa.kinds;
  const n = kinds.length;
  // Per-state "some byte fires this consumer", computed once (a
  // 256-scan per class state inside the fixpoint was the dominant
  // compile cost for class patterns).
  let fires = 0;
  for (let s = 0; s < n; s++) {
    const k = kinds[s];
    if (k === S_BYTE || k === S_ANY) fires |= 1 << s;
    else if (k === S_CLASS) {
      // One 256-scan per class state, outside the fixpoint. (A
      // positive class is almost always satisfiable, but `[\\]` on
      // Windows matches nothing — scan stays exact.)
      const cls = nfa.clsRefs[s];
      for (let b = 0; b <= 255; b++) {
        if (classMatches(cls, b)) {
          fires |= 1 << s;
          break;
        }
      }
    }
  }
  let reach = nfa.init;
  let work = reach & fires;
  while (work !== 0) {
    const s = ctz32(work);
    work &= work - 1;
    const clo = nfa.closures[nfa.nexts[s]];
    const grown = clo & ~reach;
    if (grown !== 0) {
      reach |= grown;
      work |= grown & fires;
    }
  }
  return (reach & nfa.acceptMask) !== 0;
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
    if (this.kinds.length >= MAX_SEG_NFA_STATES) return -1;
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
    if (this.nexts[state] === UNSET) this.nexts[state] = target;
  }

  compileOps(ops, dot) {
    if (ops.length === 0) {
      const s = this.alloc(S_JUMP, 0, UNSET);
      if (s === -1) return -1;
      this.tails.push(s);
      return s;
    }
    let entry = -1;
    let pending = [];
    for (const op of ops) {
      const res = this.compileOp(op, dot);
      if (res === null) return -1;
      const [opEntry, opTails] = res;
      for (const t of pending) this.patch(t, opEntry);
      pending = opTails;
      if (entry === -1) entry = opEntry;
    }
    for (const t of pending) this.tails.push(t);
    return entry;
  }

  compileOp(op, dot) {
    switch (op.kind) {
      case OP_LIT: {
        const bytes = op.bytes;
        const entry = this.litState(bytes[0]);
        if (entry === -1) return null;
        let prev = entry;
        for (let i = 1; i < bytes.length; i++) {
          const s = this.litState(bytes[i]);
          if (s === -1) return null;
          this.patch(prev, s);
          prev = s;
        }
        return [entry, [prev]];
      }
      case OP_ANYCHAR: {
        this.needsAsciiSeg = true;
        const s = this.alloc(S_ANY, 0, UNSET);
        return s === -1 ? null : [s, [s]];
      }
      case OP_CLASS: {
        if (op.cls.neg) this.needsAsciiSeg = true;
        const s = this.alloc(S_CLASS, 0, UNSET, UNSET, op.cls);
        return s === -1 ? null : [s, [s]];
      }
      case OP_STAR: {
        const entry = this.alloc(S_SPLIT, 0, UNSET);
        if (entry === -1) return null;
        const body = this.alloc(S_ANY, 0, entry);
        if (body === -1) return null;
        const exit = this.alloc(dot ? S_JUMP : S_DOT_GUARD, 0, UNSET);
        if (exit === -1) return null;
        this.nexts[entry] = body;
        this.splitBs[entry] = exit;
        return [entry, [exit]];
      }
      case OP_ALTERNATION: {
        const entries = [];
        let tails = [];
        for (const branch of op.branches) {
          const saved = this.tails;
          this.tails = [];
          const e = this.compileOps(branch, dot);
          const branchTails = this.tails;
          this.tails = saved;
          if (e === -1) return null;
          entries.push(e);
          tails = tails.concat(branchTails);
        }
        let nextState = -1;
        for (let i = op.branches.length - 2; i >= 0; i--) {
          const a = entries[i];
          const b = nextState === -1 ? entries[i + 1] : nextState;
          const s = this.alloc(S_SPLIT, 0, a, b);
          if (s === -1) return null;
          nextState = s;
        }
        return [nextState === -1 ? entries[0] : nextState, tails];
      }
      default:
        return null; // separator-crossing ops never appear in-segment
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
