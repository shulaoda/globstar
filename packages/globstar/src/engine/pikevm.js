// PikeVM. NFA simulator used when the segment matcher declines a pattern.
// State is a bitmask over a per-call `Uint32Array` of
// `nWords = ceil(nStates / 32)` words. Static ε-closures are precomputed at
// construction so byte-stepping never expands SPLIT/JUMP edges.
//
// dot=false compiles emit byte-conditional T_DOT_GUARD ε-states that static
// closures cannot absorb (whether a guard passes depends on the upcoming
// byte). Each guard's transitive pass-expansion is precomputed at
// construction; when the guard condition holds, `_run` ORs the tables of the
// active guards in one pass before the sweep. A failing guard's thread simply
// dies, since the byte switch ignores guard states.

import {
  T_BYTE,
  T_CLASS,
  T_ANY_NON_SEP,
  T_ANY_BYTE,
  T_SEP,
  T_SPLIT,
  T_JUMP,
  T_DOT_GUARD,
  T_MATCH,
  T_NULL,
  compileThompson,
} from "./thompson.js";
import { isPathSep, ctz32 } from "../options.js";
import { classMatches } from "../ast.js";
import { DirMatch } from "../dir-match.js";
import { computeStaticPrefixes } from "./ops/index.js";
import { toBytes } from "../utf8.js";

// Reach-to-accept fixed-point over byte-consumer states. reach[s] = true
// iff from state s the matcher can reach the accept state via at least
// one byte step: (closure[S.next] ∩ acceptBits ≠ ∅) OR closure[S.next]
// contains any s' with reach[s']. T_DOT_GUARD also has a `next` and
// counts. T_SPLIT / T_JUMP slots are tagged T_NULL (closures absorbed
// their ε-edges) and skipped.
function reachFromClosures(closures, infoOff, acceptOff, nWords) {
  const n = closures.length - infoOff;
  const reach = new Uint8Array(n);
  let changed = true;
  while (changed) {
    changed = false;
    for (let s = 0; s < n; s++) {
      if (reach[s]) continue;
      const word2 = closures[infoOff + s];
      const tag = word2 & 0xf;
      if (tag === T_NULL || tag === T_MATCH) continue;
      const base = (word2 >>> 16) * nWords;
      let hit = false;
      for (let w = 0; w < nWords && !hit; w++) {
        const cls = closures[base + w];
        if (cls === 0) continue;
        if (cls & closures[acceptOff + w]) {
          hit = true;
          break;
        }
        let word = cls;
        while (word !== 0) {
          const off = ctz32(word);
          const s2 = (w << 5) + off;
          word &= word - 1;
          if (reach[s2]) {
            hit = true;
            break;
          }
        }
      }
      if (hit) {
        reach[s] = 1;
        changed = true;
      }
    }
  }
  return reach;
}

// Static ε-closure as a packed Uint32Array of nWords-per-state masks
// written into `out[0 .. n*nWords)`. Caller pre-allocates `out` so the
// constructor can co-locate `initBits` + `acceptBits` after the
// closures region without paying for separate TypedArray wrappers.
//
// Input is the SoA shape from `thompson.js`: parallel arrays for tag,
// next-state, and split-second-branch. SPLIT/JUMP states are ε-only
// and walked transparently; everything else (byte-consumers, MATCH,
// DOT_GUARD) is a closure leaf and gets its bit set in `out`.
//
// Algorithm: memoized recursion over the ε-graph. Thompson's NFA keeps the
// ε-graph acyclic, since every loop in the source pattern (Star, SepRun,
// OptSegmentsSlash, ..) is broken by a byte-consumer state that is a closure
// leaf. Each state's closure then depends only on already-computed
// sub-closures, folded with a single OR into `out`. Visits each state once,
// O(n × nWords) total, versus the per-state BFS that rewalks ε-paths n times.
function staticClosuresN(tags, nexts, splitsB, n, nWords, out) {
  const computed = new Uint8Array(n);
  for (let s = 0; s < n; s++) {
    computeClosure(s, tags, nexts, splitsB, nWords, out, computed);
  }
}

function computeClosure(s, tags, nexts, splitsB, nWords, out, computed) {
  if (computed[s]) return;
  computed[s] = 1;
  const tag = tags[s];
  const base = s * nWords;
  if (tag === T_SPLIT) {
    const a = nexts[s];
    const b = splitsB[s];
    computeClosure(a, tags, nexts, splitsB, nWords, out, computed);
    computeClosure(b, tags, nexts, splitsB, nWords, out, computed);
    const aBase = a * nWords;
    const bBase = b * nWords;
    for (let w = 0; w < nWords; w++) out[base + w] = out[aBase + w] | out[bBase + w];
  } else if (tag === T_JUMP) {
    const nx = nexts[s];
    computeClosure(nx, tags, nexts, splitsB, nWords, out, computed);
    const nxBase = nx * nWords;
    for (let w = 0; w < nWords; w++) out[base + w] = out[nxBase + w];
  } else {
    // Byte-consumer / DOT_GUARD / MATCH: closure(s) = {s}.
    const w = s >>> 5;
    const bit = 1 << (s & 31);
    out[base + w] = bit;
  }
}

export class PikeVm {
  constructor(nfa, facts, prefixes) {
    this.facts = facts;
    this.prefixes = prefixes;

    const n = nfa.n;
    const nWords = Math.max(1, (n + 31) >>> 5);
    this.nWords = nWords;

    // Co-allocate closures + initBits + acceptBits + per-state `info`
    // into one `Uint32Array`. Saves three TypedArray wrappers + their
    // backing-store metadata vs separate arrays. Layout (each closure /
    // bits region is `nWords` words, the `info` region is `n` words, one
    // packed word per state):
    //   [0,                       n*nWords)    closures (inner-loop hot)
    //   [n*nWords,            (n+1)*nWords)    initBits (read once/match)
    //   [(n+1)*nWords,        (n+2)*nWords)    acceptBits (read once/match)
    //   [(n+2)*nWords, (n+2)*nWords + n)       info (per state, hot loop)
    //
    // Each state's `info` word packs:
    //   bits  0..3   tag      (T_MATCH..T_NULL = 0..9 fit in 4 bits)
    //   bits  4..7   flags    (bit 0 = dotProtected)
    //   bits  8..15  byte     (T_BYTE byte value)
    //   bits 16..31  next     (next-state index, ≤ 65535)
    // ε-only T_SPLIT / T_JUMP slots get tag = T_NULL. Closures already
    // absorbed their edges, so they're never consumed at byte-step.
    // `clsRefs` stays an Object array (only ~10% of states use it, and
    // `cls` is a structured value we can't inline into bits).
    const initOff = n * nWords;
    const acceptOff = initOff + nWords;
    const infoOff = acceptOff + nWords;
    const combined = new Uint32Array(infoOff + n);
    staticClosuresN(nfa.tags, nfa.nexts, nfa.splitsB, n, nWords, combined);
    const initBase = nfa.initial * nWords;
    for (let w = 0; w < nWords; w++) combined[initOff + w] = combined[initBase + w];
    const eof = nfa.acceptsAtEof;
    for (let i = 0; i < eof.length; i++) {
      if (eof[i]) combined[acceptOff + (i >>> 5)] |= 1 << (i & 31);
    }
    const tags = nfa.tags;
    const nexts = nfa.nexts;
    const byteVals = nfa.byteVals;
    const flagBits = nfa.flagBits;
    const inClsRefs = nfa.clsRefs;
    let clsRefs = null;
    const guardIds = [];
    for (let i = 0; i < n; i++) {
      const tag = tags[i];
      if (tag === T_SPLIT || tag === T_JUMP) {
        combined[infoOff + i] = T_NULL;
        continue;
      }
      combined[infoOff + i] = tag | (flagBits[i] << 4) | (byteVals[i] << 8) | (nexts[i] << 16);
      if (tag === T_DOT_GUARD) guardIds.push(i);
      if (tag === T_CLASS) {
        if (clsRefs === null) clsRefs = Array.from({ length: n });
        clsRefs[i] = inClsRefs[i];
      }
    }
    this.closures = combined;
    this.initOff = initOff;
    this.acceptOff = acceptOff;
    this.infoOff = infoOff;
    this.clsRefs = clsRefs;

    // Packed T_DOT_GUARD pass-expansions, one record of `1 + nWords` words
    // per guard: the guard's state id, then the bitmap it releases when it
    // passes (its target's closure, with chained guards' expansions folded
    // in). The fold makes records transitive, so `_run` expands every active
    // guard in one pass. Null for dot=true compiles.
    let guardExps = null;
    if (guardIds.length > 0) {
      const stride = 1 + nWords;
      guardExps = new Uint32Array(guardIds.length * stride);
      for (let i = 0; i < guardIds.length; i++) {
        const g = guardIds[i];
        guardExps[i * stride] = g;
        const base = nexts[g] * nWords;
        for (let w = 0; w < nWords; w++) guardExps[i * stride + 1 + w] = combined[base + w];
      }
      // Fold guard chains (a guard whose expansion holds another guard)
      // until stable.
      let changed = true;
      while (changed) {
        changed = false;
        for (let i = 0; i < guardIds.length; i++) {
          for (let j = 0; j < guardIds.length; j++) {
            const gj = guardIds[j];
            if (i === j || (guardExps[i * stride + 1 + (gj >>> 5)] & (1 << (gj & 31))) === 0) {
              continue;
            }
            for (let w = 0; w < nWords; w++) {
              // `>>> 0` forces unsigned: the OR yields a signed int32, but
              // Uint32Array reads are unsigned, and a signed/unsigned compare
              // with bit 31 set would never stabilize.
              const merged = (guardExps[i * stride + 1 + w] | guardExps[j * stride + 1 + w]) >>> 0;
              if (merged !== guardExps[i * stride + 1 + w]) {
                guardExps[i * stride + 1 + w] = merged;
                changed = true;
              }
            }
          }
        }
      }
    }
    this.guardExps = guardExps;

    // Scratch buffers reused across match calls (single-threaded), packed
    // into one Uint32Array. Layout (nWords each):
    //   [0,         nWords)    cur
    //   [nWords,  2*nWords)    nxt
    this._scratch = new Uint32Array(nWords * 2);

    // Lazy reach-to-accept (matchDir prefix mode). Computed on the first
    // matchDir call from `closures` + `acceptBits`. The compile-time `nfa`
    // SoA is not retained, so its tags/nexts/splitsB drop out of scope here.
    this._reachToAccept = null;
  }

  static build(program, dot) {
    const nfa = compileThompson(program, dot);
    return new PikeVm(nfa, program.facts, computeStaticPrefixes(program.ops));
  }

  staticPrefixes() {
    return this.prefixes;
  }

  _isAccept(bits) {
    const closures = this.closures;
    const acceptOff = this.acceptOff;
    const nWords = this.nWords;
    for (let w = 0; w < nWords; w++) {
      if (bits[w] & closures[acceptOff + w]) return true;
    }
    return false;
  }

  isMatch(input) {
    const path = toBytes(input);
    if (!this.facts.accept(path)) return false;
    this._run(path);
    return this._isAccept(this._scratch);
  }

  matchDir(input) {
    const dirPath = toBytes(input);
    // Empty dir path is the cwd and every match lives under it, so descent
    // is always on. The prefix probe would instead simulate a leading `/`,
    // which cwd children don't have.
    if (dirPath.length === 0) return DirMatch.fromExactPrefix(this.isMatch(""), true);
    this._run(dirPath);
    return DirMatch.fromExactPrefix(this._isAccept(this._scratch), this._hasPrefixDescent());
  }

  _hasPrefixDescent() {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const nWords = this.nWords;
    const scratch = this._scratch;

    // The separator step below consumes a `/`, never a segment-start dot, so
    // every `T_DOT_GUARD` in the live set passes here. Static closures stop at
    // a dot-guard (a byte-conditional ε-leaf), so a separator consumer behind
    // one (e.g. the `SEP` of `*/` under `dot=false`) would otherwise be
    // invisible to the raw `scratch` scan and the subtree wrongly pruned.
    const cur = new Uint32Array(nWords);
    for (let w = 0; w < nWords; w++) cur[w] = scratch[w];
    const guardExps = this.guardExps;
    if (guardExps !== null) {
      const stride = 1 + nWords;
      for (let r = 0; r < guardExps.length; r += stride) {
        const g = guardExps[r];
        if (cur[g >>> 5] & (1 << (g & 31))) {
          for (let j = 0; j < nWords; j++) cur[j] |= guardExps[r + 1 + j];
        }
      }
    }

    const next = new Uint32Array(nWords);
    for (let w = 0; w < nWords; w++) {
      let word = cur[w];
      while (word !== 0) {
        const off = ctz32(word);
        const s = (w << 5) + off;
        word &= word - 1;
        const word2 = closures[infoOff + s];
        const tag = word2 & 0xf;
        if (tag === T_SEP || tag === T_ANY_BYTE) {
          const base = (word2 >>> 16) * nWords;
          for (let j = 0; j < nWords; j++) next[j] |= closures[base + j];
        }
      }
    }
    if (this._isAccept(next)) return true;
    let reach = this._reachToAccept;
    if (reach === null) {
      reach = reachFromClosures(closures, infoOff, this.acceptOff, nWords);
      this._reachToAccept = reach;
    }
    for (let w = 0; w < nWords; w++) {
      let word = next[w];
      while (word !== 0) {
        const off = ctz32(word);
        const s = (w << 5) + off;
        word &= word - 1;
        if (reach[s]) return true;
      }
    }
    return false;
  }

  _run(path) {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const clsRefs = this.clsRefs;
    const nWords = this.nWords;
    const scratch = this._scratch;
    const guardExps = this.guardExps;
    const stride = 1 + nWords;
    // cur slice: scratch[0..nWords)   nxt slice: scratch[nWords..2*nWords)
    const nxtBase = nWords;
    const initOff = this.initOff;
    for (let w = 0; w < nWords; w++) scratch[w] = closures[initOff + w];

    let atSegStart = true;

    for (let p = 0; p < path.length; p++) {
      const c = path[p];
      const sep = isPathSep(c);
      // `info[s]` packs `dotProtected` at bit 4 (0x10). At a segment-start
      // dot, `dotMaskFlag = 0x10` so `!(word2 & dotMaskFlag)` rejects
      // dot-protected states; otherwise it's `0` and always passes.
      const dotMaskFlag = atSegStart && c === 0x2e ? 0x10 : 0;

      // Passing guards release their precomputed transitive expansions; a
      // failing guard's thread dies in the byte switch below.
      if (guardExps !== null && dotMaskFlag === 0) {
        for (let r = 0; r < guardExps.length; r += stride) {
          const g = guardExps[r];
          if (scratch[g >>> 5] & (1 << (g & 31))) {
            for (let j = 0; j < nWords; j++) scratch[j] |= guardExps[r + 1 + j];
          }
        }
      }

      for (let w = 0; w < nWords; w++) scratch[nxtBase + w] = 0;

      for (let w = 0; w < nWords; w++) {
        let word = scratch[w];
        while (word !== 0) {
          const off = ctz32(word);
          const s = (w << 5) + off;
          word &= word - 1;
          const word2 = closures[infoOff + s];
          let matched = false;
          switch (word2 & 0xf) {
            case T_BYTE:
              matched = ((word2 >>> 8) & 0xff) === c;
              break;
            case T_CLASS:
              matched = classMatches(clsRefs[s], c) && !(word2 & dotMaskFlag);
              break;
            case T_ANY_NON_SEP:
              matched = !sep && !(word2 & dotMaskFlag);
              break;
            case T_ANY_BYTE:
              matched = !(word2 & dotMaskFlag);
              break;
            case T_SEP:
              matched = sep;
              break;
          }
          if (matched) {
            const base = (word2 >>> 16) * nWords;
            for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
          }
        }
      }

      // Copy nxt → cur in scratch.
      let allZero = true;
      for (let w = 0; w < nWords; w++) {
        const v = scratch[nxtBase + w];
        scratch[w] = v;
        if (v !== 0) allZero = false;
      }
      if (allZero) return;
      atSegStart = sep;
    }
  }
}
