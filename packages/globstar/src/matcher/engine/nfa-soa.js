// SoA-direct NFA builder for PikeVm. Mirrors `thompson.js`'s build
// algorithm but stores state data in parallel `number[]` arrays
// instead of a `Trans` object per state — Builder skips ~N V8 object
// allocations per compile, freezes to typed arrays at the end, and
// hands PikeVm a ready-to-consume SoA shape.
//
// Why a separate file: `thompson.js` is still the source of truth
// for the DFA path (the DFA build loop reads `t.tag/t.b/t.next/t.a/
// t.splitB/t.dotProtected/t.cls` extensively). Touching it to share
// storage would force a parallel DFA refactor. This module is
// PikeVm-only and lets the DFA path stay byte-for-byte unchanged.
//
// Per-state storage (during build, plain JS arrays):
//   tags[s]      number   T_BYTE | T_CLASS | …
//   nexts[s]     number   for byte-consumers/JUMP/DOT_GUARD: next state;
//                          for SPLIT: first branch (`a`)
//   byteVals[s]  number   T_BYTE byte value (else 0)
//   flagBits[s]  number   bit 0 = dotProtected (else 0)
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
//   initial, acceptsAtEof, n.

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
} from "./ops.js";
import { CI_BYTE } from "../ast.js";
import { asciiCaseAlt } from "../options.js";
import {
  T_MATCH,
  T_BYTE,
  T_CLASS,
  T_ANY_NON_SEP,
  T_ANY_BYTE,
  T_SEP,
  T_SPLIT,
  T_JUMP,
  T_DOT_GUARD,
} from "./thompson.js";

const UNSET = -1;

class Builder {
  constructor(caseInsensitive) {
    this.caseInsensitive = caseInsensitive;
    this.tags = [];
    this.nexts = [];
    this.byteVals = [];
    this.flagBits = [];
    this.splitsB = [];
    this.clsRefs = []; // sparse — null entries common
    this.tailPatches = [];
  }

  _push(tag, next, byteVal, flags, cls, splitB) {
    const id = this.tags.length;
    this.tags.push(tag);
    this.nexts.push(next);
    this.byteVals.push(byteVal);
    this.flagBits.push(flags);
    this.splitsB.push(splitB);
    this.clsRefs.push(cls);
    return id;
  }

  allocByte(b) {
    return this._push(T_BYTE, UNSET, b, 0, null, UNSET);
  }
  allocClass(cls, dotProtected) {
    return this._push(T_CLASS, UNSET, 0, dotProtected ? 1 : 0, cls, UNSET);
  }
  allocAnyNonSep(next, dotProtected) {
    return this._push(T_ANY_NON_SEP, next, 0, dotProtected ? 1 : 0, null, UNSET);
  }
  allocAnyByte(next, dotProtected) {
    return this._push(T_ANY_BYTE, next, 0, dotProtected ? 1 : 0, null, UNSET);
  }
  allocSep(next) {
    return this._push(T_SEP, next, 0, 0, null, UNSET);
  }
  allocSplit(a, splitB) {
    return this._push(T_SPLIT, a, 0, 0, null, splitB);
  }
  allocJump(next) {
    return this._push(T_JUMP, next, 0, 0, null, UNSET);
  }
  allocDotGuard(next) {
    return this._push(T_DOT_GUARD, next, 0, 0, null, UNSET);
  }
  allocMatch() {
    return this._push(T_MATCH, 0, 0, 0, null, UNSET);
  }

  allocLitByte(b) {
    const alt = asciiCaseAlt(b);
    if (this.caseInsensitive && alt !== b) {
      const cls = {
        neg: false,
        items: [
          { tag: CI_BYTE, b },
          { tag: CI_BYTE, b: alt },
        ],
      };
      return this.allocClass(cls, false);
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

  compileOps(ops, dot) {
    if (ops.length === 0) {
      const s = this.allocJump(UNSET);
      this.tailPatches.push(s);
      return s;
    }
    let entry = -1;
    let pendingTails = [];
    for (const op of ops) {
      const [opEntry, opTails] = this.compileOp(op, dot);
      for (const t of pendingTails) this.patch(t, opEntry);
      pendingTails = opTails;
      if (entry === -1) entry = opEntry;
    }
    for (const t of pendingTails) this.tailPatches.push(t);
    return entry;
  }

  compileOp(op, dot) {
    switch (op.kind) {
      case OP_LIT:
        return this.compileLit(op.bytes);
      case OP_ANYCHAR:
        return this.compileAnyNonSep(dot);
      case OP_STAR:
        return this.compileStar(dot);
      case OP_CLASS:
        return this.compileClass(op.cls, dot);
      case OP_SEP:
        return this.compileSep();
      case OP_SEP_RUN:
        return this.compileSepRun();
      case OP_LEADING_SEPS:
        return this.compileLeadingSeps();
      case OP_OPT_SEGMENTS_SLASH:
        return this.compileOss(dot);
      case OP_SLASH_ANYTHING:
        return this.compileSlashAnything(dot);
      case OP_GLOBSTAR_ANY:
        return this.compileGlobstarAny(dot);
      case OP_ALTERNATION:
        return this.compileAlternation(op.branches, dot);
      case OP_GLOBSTAR: {
        const s = this.allocByte(0);
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

  compileAnyNonSep(dot) {
    const s = this.allocAnyNonSep(UNSET, !dot);
    return [s, [s]];
  }

  compileStar(dot) {
    const entry = this.allocSplit(UNSET, UNSET);
    const body = this.allocAnyNonSep(entry, !dot);
    const dotGuard = !dot ? this.allocDotGuard(UNSET) : this.allocJump(UNSET);
    this.nexts[entry] = body;
    this.splitsB[entry] = dotGuard;
    return [entry, [dotGuard]];
  }

  compileClass(cls, dot) {
    const s = this.allocClass(cls, !dot && cls.neg);
    return [s, [s]];
  }

  compileSep() {
    const entry = this.allocSep(UNSET);
    return [entry, [entry]];
  }

  compileSepRun() {
    const tailSplit = this.allocSplit(UNSET, UNSET);
    const loopBody = this.allocSep(tailSplit);
    this.nexts[tailSplit] = loopBody;
    const entry = this.allocSep(tailSplit);
    return [entry, [tailSplit]];
  }

  compileLeadingSeps() {
    const entry = this.allocSplit(UNSET, UNSET);
    const loopBody = this.allocSep(entry);
    this.nexts[entry] = loopBody;
    return [entry, [entry]];
  }

  compileOss(dot) {
    const entry = this.allocSplit(UNSET, UNSET);
    const segBody = this.allocAnyNonSep(UNSET, !dot);
    const segCont = this.allocSplit(UNSET, UNSET);
    const segBodyLoop = this.allocAnyNonSep(segCont, false);
    const sepStart = this.allocSep(UNSET);
    const sepTail = this.allocSplit(UNSET, UNSET);
    const sepLoop = this.allocSep(sepTail);

    this.nexts[segBody] = segCont;
    this.nexts[segCont] = segBodyLoop;
    this.splitsB[segCont] = sepStart;
    this.nexts[sepStart] = sepTail;
    this.nexts[sepTail] = sepLoop;
    this.splitsB[sepTail] = entry;
    this.nexts[entry] = segBody;
    return [entry, [entry]];
  }

  compileSlashAnything(dot) {
    const entry = this.allocSep(UNSET);
    const postSep = this.allocSplit(UNSET, UNSET);
    const sepLoop = this.allocSep(postSep);
    const tail = this.allocSplit(UNSET, UNSET);
    const tailLoop = this.allocAnyByte(tail, !dot);

    this.nexts[entry] = postSep;
    this.nexts[postSep] = sepLoop;
    this.splitsB[postSep] = tail;
    this.nexts[tail] = tailLoop;
    return [entry, [tail]];
  }

  compileGlobstarAny(dot) {
    const entry = this.allocSplit(UNSET, UNSET);
    const body = this.allocAnyByte(entry, !dot);
    this.nexts[entry] = body;
    return [entry, [entry]];
  }

  compileAlternation(branches, dot) {
    if (branches.length === 1) {
      const entry = this.compileOps(branches[0], dot);
      const tails = this.tailPatches;
      this.tailPatches = [];
      return [entry, tails];
    }
    const branchEntries = [];
    const branchTails = [];
    for (const branch of branches) {
      const entry = this.compileOps(branch, dot);
      const tails = this.tailPatches;
      this.tailPatches = [];
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
export function compileNfaSoa(program, dot) {
  const builder = new Builder(program.caseInsensitive);
  const initial = builder.allocJump(UNSET);
  const bodyEntry = builder.compileOps(program.ops, dot);
  const accept = builder.allocMatch();
  builder.patch(initial, bodyEntry);
  for (const st of builder.tailPatches) builder.patch(st, accept);

  const n = builder.tags.length;
  const tags = builder.tags;
  const nexts = builder.nexts;
  const splitsB = builder.splitsB;
  const byteVals = builder.byteVals;
  const flagBits = builder.flagBits;
  const clsRefs = builder.clsRefs;
  const acceptsAtEof = computeAcceptsAtEof(tags, nexts, splitsB, n);

  return {
    n,
    tags,
    nexts,
    splitsB,
    byteVals,
    flagBits,
    clsRefs,
    initial,
    acceptsAtEof,
  };
}

// Same fixed-point as thompson.js's `computeAcceptsAtEof`, but reads
// SoA arrays. ε-only states (T_SPLIT / T_JUMP / T_DOT_GUARD) propagate
// the "reaches accept on no more bytes" property through their ε-edges.
function computeAcceptsAtEof(tags, nexts, splitsB, n) {
  const acc = new Uint8Array(n);
  for (let i = 0; i < n; i++) if (tags[i] === T_MATCH) acc[i] = 1;
  let changed = true;
  while (changed) {
    changed = false;
    for (let i = 0; i < n; i++) {
      if (acc[i]) continue;
      const tag = tags[i];
      let reaches = false;
      if (tag === T_JUMP || tag === T_DOT_GUARD) {
        reaches = !!acc[nexts[i]];
      } else if (tag === T_SPLIT) {
        reaches = !!acc[nexts[i]] || !!acc[splitsB[i]];
      }
      if (reaches) {
        acc[i] = 1;
        changed = true;
      }
    }
  }
  return acc;
}
