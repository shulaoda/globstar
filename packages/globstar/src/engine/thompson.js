// Struct-of-arrays Thompson NFA builder for the PikeVM. JS port of the
// Rust module `crates/globstar/src/engine/thompson.rs`. Compiles to
// parallel `number[]` arrays
// instead of a `Trans` object per state — Builder skips ~N V8 object
// allocations per compile, freezes to typed arrays at the end, and
// hands PikeVm a ready-to-consume SoA shape.
//
// Per-state storage (during build, plain JS arrays):
//   tags[s]      number   T_BYTE | T_CLASS | …
//   nexts[s]     number   for byte-consumers/JUMP/DOT_GUARD: next state;
//                          for SPLIT: first branch (`a`)
//   byteVals[s]  number   T_BYTE byte value (else 0)
//   splitsB[s]  number   T_SPLIT second branch (else UNSET)
//   clsRefs[s]   Object   T_CLASS class struct (else null)
//
// Frozen output (consumed by PikeVm constructor):
//   info: Uint32Array(n) — packed tag/flags/byte/next per byte-consumer
//                          state (T_SPLIT/T_JUMP slots get tag=T_NULL,
//                          edges already absorbed by closures).
//   clsRefs: Array(n) | null
//   tags, nexts, splitsB: kept as number[] for `staticClosuresN` —
//                          dropped after closure computation.
//   initial, accept, n.

import {
  OP_LIT,
  OP_ANYCHAR,
  OP_STAR,
  OP_CLASS,
  OP_SEP,
  OP_SEP_RUN,
  OP_GLOBSTAR,
  OP_OPT_SEGMENTS_SLASH,
  OP_SLASH_ANYTHING,
  OP_GLOBSTAR_ANY,
  OP_LEADING_SEPS,
  OP_ALTERNATION,
} from "./ops/index.js";
import { ciLetter } from "../ast.js";

// NFA transition tags. `T_NULL` is a packed-runtime sentinel for ε-only
// states whose closures have already been absorbed by PikeVm.
export const T_MATCH = 0;
export const T_BYTE = 1;
export const T_CLASS = 2;
export const T_ANY_NON_SEP = 3;
export const T_ANY_BYTE = 4;
export const T_SEP = 5;
export const T_SPLIT = 6;
export const T_JUMP = 7;
export const T_DOT_GUARD = 8;
export const T_NULL = 9;

const UNSET = -1;

class Builder {
  constructor(caseInsensitive, dot) {
    this.caseInsensitive = caseInsensitive;
    // When false, `*` guards its zero-match exit with a T_DOT_GUARD.
    this.dot = dot;
    this.tags = [];
    this.nexts = [];
    this.byteVals = [];
    this.splitsB = [];
    this.clsRefs = []; // sparse — null entries common
  }

  _push(tag, next, byteVal, cls, splitB) {
    const id = this.tags.length;
    this.tags.push(tag);
    this.nexts.push(next);
    this.byteVals.push(byteVal);
    this.splitsB.push(splitB);
    this.clsRefs.push(cls);
    return id;
  }

  allocByte(b) {
    return this._push(T_BYTE, UNSET, b, null, UNSET);
  }
  allocClass(cls) {
    return this._push(T_CLASS, UNSET, 0, cls, UNSET);
  }
  allocAnyNonSep(next) {
    return this._push(T_ANY_NON_SEP, next, 0, null, UNSET);
  }
  allocAnyByte(next) {
    return this._push(T_ANY_BYTE, next, 0, null, UNSET);
  }
  allocSep(next) {
    return this._push(T_SEP, next, 0, null, UNSET);
  }
  allocSplit(a, splitB) {
    return this._push(T_SPLIT, a, 0, null, splitB);
  }
  allocJump(next) {
    return this._push(T_JUMP, next, 0, null, UNSET);
  }
  allocDotGuard(next) {
    return this._push(T_DOT_GUARD, next, 0, null, UNSET);
  }
  allocMatch() {
    return this._push(T_MATCH, 0, 0, null, UNSET);
  }

  allocLitByte(b) {
    if (this.caseInsensitive) {
      const cls = ciLetter(b);
      if (cls !== null) return this.allocClass(cls);
    }
    return this.allocByte(b);
  }

  patch(state, target) {
    const tag = this.tags[state];
    if (tag === T_MATCH) throw new Error("cannot patch a Match state");
    if (tag === T_SPLIT) {
      if (this.nexts[state] === UNSET) this.nexts[state] = target;
      else if (this.splitsB[state] === UNSET) this.splitsB[state] = target;
      return;
    }
    // BYTE / CLASS / ANY_NON_SEP / ANY_BYTE / SEP / JUMP / DOT_GUARD
    if (this.nexts[state] === UNSET) this.nexts[state] = target;
  }

  compileOps(ops) {
    if (ops.length === 0) {
      const s = this.allocJump(UNSET);
      return [s, [s]];
    }
    let entry = -1;
    let pendingTails = [];
    for (const op of ops) {
      const [opEntry, opTails] = this.compileOp(op);
      for (const t of pendingTails) this.patch(t, opEntry);
      pendingTails = opTails;
      if (entry === -1) entry = opEntry;
    }
    return [entry, pendingTails];
  }

  compileOp(op) {
    switch (op.kind) {
      case OP_LIT:
        return this.compileLit(op.bytes);
      case OP_ANYCHAR:
        return this.compileAnyNonSep();
      case OP_STAR:
        return this.compileStar();
      case OP_CLASS:
        return this.compileClass(op.cls);
      case OP_SEP:
        return this.compileSep();
      case OP_SEP_RUN:
        return this.compileSepRun();
      case OP_LEADING_SEPS:
        return this.compileLeadingSeps();
      case OP_OPT_SEGMENTS_SLASH:
        return this.compileOss();
      case OP_SLASH_ANYTHING:
        return this.compileSlashAnything();
      case OP_GLOBSTAR_ANY:
        return this.compileGlobstarAny();
      case OP_ALTERNATION:
        return this.compileAlternation(op.branches);
      case OP_GLOBSTAR: {
        // Lowering folds raw globstars away. Release safety net: an empty
        // class matches no byte, so a leak can never produce a false match.
        const s = this.allocClass({ neg: false, items: [] });
        return [s, [s]];
      }
    }
    throw new Error("compileOp: unreachable");
  }

  compileLit(bytes) {
    const entry = this.allocLitByte(bytes[0]);
    let prev = entry;
    for (let i = 1; i < bytes.length; i++) {
      const s = this.allocLitByte(bytes[i]);
      this.patch(prev, s);
      prev = s;
    }
    return [entry, [prev]];
  }

  compileAnyNonSep() {
    const s = this.allocAnyNonSep(UNSET);
    return [s, [s]];
  }

  compileStar() {
    const entry = this.allocSplit(UNSET, UNSET);
    const body = this.allocAnyNonSep(entry);
    this.nexts[entry] = body;
    if (!this.dot) {
      const dotGuard = this.allocDotGuard(UNSET);
      this.splitsB[entry] = dotGuard;
      return [entry, [dotGuard]];
    }
    // No dot protection: the Split's dangling exit is the fragment tail.
    return [entry, [entry]];
  }

  compileClass(cls) {
    const s = this.allocClass(cls);
    return [s, [s]];
  }

  compileSep() {
    const entry = this.allocSep(UNSET);
    return [entry, [entry]];
  }

  compileSepRun() {
    const tailSplit = this.allocSplit(UNSET, UNSET);
    const entry = this.allocSep(tailSplit);
    this.nexts[tailSplit] = entry;
    return [entry, [tailSplit]];
  }

  compileLeadingSeps() {
    const entry = this.allocSplit(UNSET, UNSET);
    const loopBody = this.allocSep(entry);
    this.nexts[entry] = loopBody;
    return [entry, [entry]];
  }

  compileOss() {
    const entry = this.allocSplit(UNSET, UNSET);
    const segBody = this.allocAnyNonSep(UNSET);
    const segCont = this.allocSplit(UNSET, UNSET);
    const segBodyLoop = this.allocAnyNonSep(segCont);
    const sepStart = this.allocSep(UNSET);
    const sepTail = this.allocSplit(UNSET, UNSET);

    this.nexts[segBody] = segCont;
    this.nexts[segCont] = segBodyLoop;
    this.splitsB[segCont] = sepStart;
    this.nexts[sepStart] = sepTail;
    this.nexts[sepTail] = sepStart;
    this.splitsB[sepTail] = entry;
    this.nexts[entry] = segBody;
    return [entry, [entry]];
  }

  compileSlashAnything() {
    const entry = this.allocSep(UNSET);
    const postSep = this.allocSplit(UNSET, UNSET);
    const tail = this.allocSplit(UNSET, UNSET);
    const tailLoop = this.allocAnyByte(tail);

    this.nexts[entry] = postSep;
    this.nexts[postSep] = entry;
    this.splitsB[postSep] = tail;
    this.nexts[tail] = tailLoop;
    return [entry, [tail]];
  }

  compileGlobstarAny() {
    const entry = this.allocSplit(UNSET, UNSET);
    const body = this.allocAnyByte(entry);
    this.nexts[entry] = body;
    return [entry, [entry]];
  }

  // Every constructible Alternation has >= 2 branches: the parser collapses
  // single-branch braces into literals (GLOB_SPEC §7.4) and the union
  // factorer only wraps >= 2 patterns.
  compileAlternation(branches) {
    const branchEntries = [];
    const branchTails = [];
    for (const branch of branches) {
      const [entry, tails] = this.compileOps(branch);
      branchEntries.push(entry);
      for (const t of tails) branchTails.push(t);
    }
    let nextState = -1;
    for (let i = branches.length - 2; i >= 0; i--) {
      const a = branchEntries[i];
      const b = nextState !== -1 ? nextState : branchEntries[i + 1];
      const s = this.allocSplit(a, b);
      nextState = s;
    }
    return [nextState, branchTails];
  }
}

// Build the SoA-form NFA. Returns the working data PikeVm needs to
// finish constructing its run state (closures, scratch, etc.).
export function compileThompson(program, dot) {
  const builder = new Builder(program.caseInsensitive, dot);
  const [initial, tails] = builder.compileOps(program.ops);
  const accept = builder.allocMatch();
  for (const st of tails) builder.patch(st, accept);

  const n = builder.tags.length;
  const tags = builder.tags;
  const nexts = builder.nexts;
  const splitsB = builder.splitsB;
  const byteVals = builder.byteVals;
  const clsRefs = builder.clsRefs;
  return {
    n,
    tags,
    nexts,
    splitsB,
    byteVals,
    clsRefs,
    initial,
    accept,
    dot,
  };
}
