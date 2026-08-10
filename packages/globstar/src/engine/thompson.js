import {
  OP_LIT,
  OP_ANYCHAR,
  OP_STAR,
  OP_CLASS,
  OP_SEP,
  OP_SEP_RUN,
  OP_OPT_SEGMENTS_SLASH,
  OP_SLASH_ANYTHING,
  OP_GLOBSTAR_ANY,
  OP_LEADING_SEPS,
  OP_ALTERNATION,
} from "./ops/index.js";
import { ciLetter } from "../ast.js";

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
    this.dot = dot;
    this.tags = [];
    this.nexts = [];
    this.byteVals = [];
    this.splitsB = [];
    this.clsRefs = [];
  }

  _push(tag, next, byteVal, cls, splitB) {
    const id = this.nextId();
    this.tags.push(tag);
    this.nexts.push(next);
    this.byteVals.push(byteVal);
    this.splitsB.push(splitB);
    this.clsRefs.push(cls);
    return id;
  }

  nextId() {
    return this.tags.length;
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
    if (this.nexts[state] === UNSET) this.nexts[state] = target;
  }

  compileOps(ops) {
    if (ops.length === 0) {
      const s = this.allocJump(UNSET);
      return [s, [s]];
    }
    const [entry, tails] = this.compileOp(ops[0]);
    let pendingTails = tails;
    for (let i = 1; i < ops.length; i++) {
      const [opEntry, opTails] = this.compileOp(ops[i]);
      for (const t of pendingTails) this.patch(t, opEntry);
      pendingTails = opTails;
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
    const entry = this.nextId();
    const body = entry + 1;
    if (this.dot) {
      this.allocSplit(body, UNSET);
      this.allocAnyNonSep(entry);
      return [entry, [entry]];
    }
    const dotGuard = entry + 2;
    this.allocSplit(body, dotGuard);
    this.allocAnyNonSep(entry);
    this.allocDotGuard(UNSET);
    return [entry, [dotGuard]];
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
    const tailSplit = this.nextId();
    const entry = tailSplit + 1;
    this.allocSplit(entry, UNSET);
    this.allocSep(tailSplit);
    return [entry, [tailSplit]];
  }

  compileLeadingSeps() {
    const entry = this.nextId();
    this.allocSplit(entry + 1, UNSET);
    this.allocSep(entry);
    return [entry, [entry]];
  }

  compileOss() {
    const entry = this.nextId();
    const [segBody, segCont, segBodyLoop, sepStart, sepTail] = [
      entry + 1,
      entry + 2,
      entry + 3,
      entry + 4,
      entry + 5,
    ];
    this.allocSplit(segBody, UNSET);
    this.allocAnyNonSep(segCont);
    this.allocSplit(segBodyLoop, sepStart);
    this.allocAnyNonSep(segCont);
    this.allocSep(sepTail);
    this.allocSplit(sepStart, entry);
    return [entry, [entry]];
  }

  compileSlashAnything() {
    const entry = this.nextId();
    const [postSep, tail, tailLoop] = [entry + 1, entry + 2, entry + 3];
    this.allocSep(postSep);
    this.allocSplit(entry, tail);
    this.allocSplit(tailLoop, UNSET);
    this.allocAnyByte(tail);
    return [entry, [tail]];
  }

  compileGlobstarAny() {
    const entry = this.nextId();
    this.allocSplit(entry + 1, UNSET);
    this.allocAnyByte(entry);
    return [entry, [entry]];
  }

  compileAlternation(branches) {
    const branchEntries = [];
    const branchTails = [];
    for (const branch of branches) {
      const [entry, tails] = this.compileOps(branch);
      branchEntries.push(entry);
      for (const t of tails) branchTails.push(t);
    }
    let entry = branchEntries[branchEntries.length - 1];
    for (let i = branchEntries.length - 2; i >= 0; i--) {
      entry = this.allocSplit(branchEntries[i], entry);
    }
    return [entry, branchTails];
  }
}

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
