// Trans tags.
export const T_MATCH = 0;
export const T_BYTE = 1;
export const T_CLASS = 2;
export const T_ANY_NON_SEP = 3;
export const T_ANY_BYTE = 4;
export const T_SEP = 5;
export const T_SPLIT = 6;
export const T_JUMP = 7;
export const T_DOT_GUARD = 8;
// Sentinel used by SoA consumers (PikeVm) to mark slots whose original
// state was an ε-only T_SPLIT / T_JUMP — these never appear in the
// active set after `staticClosuresN`, but their slot still exists so
// state indices stay stable across the conversion.
export const T_NULL = 9;

// Single source of truth for NFA construction is `nfa-soa.js`.
// `Thompson.compile` adapts its SoA output back into the Object-array
// shape the DFA build path consumes (`states[s].tag/next/b/.../a/splitB`).
// PikeVm reads SoA directly and never lands here.
import { compileNfaSoa } from "./nfa-soa.js";

export class Thompson {
  constructor(states, initial, acceptsAtEof) {
    this.states = states;
    this.initial = initial;
    this.acceptsAtEof = acceptsAtEof;
  }

  static compile(program, dot) {
    const nfa = compileNfaSoa(program, dot);
    const n = nfa.n;
    const tags = nfa.tags;
    const nexts = nfa.nexts;
    const splitsB = nfa.splitsB;
    const byteVals = nfa.byteVals;
    const flagBits = nfa.flagBits;
    const clsRefs = nfa.clsRefs;
    const states = Array.from({ length: n });
    for (let i = 0; i < n; i++) {
      const tag = tags[i];
      // For T_SPLIT, `nexts[i]` holds the first branch (`a`) and
      // `splitsB[i]` holds the second; non-split states use `nexts[i]`
      // as their lone `next`. Reconstruct the legacy shape so the DFA
      // build loops keep reading `t.a / t.splitB / t.next` unchanged.
      const isSplit = tag === T_SPLIT;
      states[i] = {
        tag,
        next: isSplit ? 0 : nexts[i],
        b: byteVals[i],
        dotProtected: flagBits[i],
        cls: clsRefs[i],
        a: isSplit ? nexts[i] : 0,
        splitB: isSplit ? splitsB[i] : 0,
      };
    }
    return new Thompson(states, nfa.initial, nfa.acceptsAtEof);
  }
}
