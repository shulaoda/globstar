// PikeVM — NFA simulator used by the matcher when the DFA path can't
// fit in its state cap. Bitmask state representation backed by a
// per-call `Uint32Array` of `nWords = ceil(nStates / 32)` entries; one
// 32-bit set-bit iteration per word. Static ε-closures are
// pre-computed at construction so byte-stepping never needs to
// expand SPLIT/JUMP edges.
//
// Two byte-step variants picked at construction:
//   - `_runFast`     — single pass, no work queue. Used when the NFA
//                       has no `T_DOT_GUARD` (i.e. `dot=true`).
//   - `_runDotGuard` — work-queue with `processed` dedup, handles the
//                       byte-conditional ε of `T_DOT_GUARD`. Used when
//                       `dot=false` and the NFA has `T_DOT_GUARD`.
//
// Replaced the previous "Wide" array-based PikeVM (commit history) —
// per-byte cost is 3-5x lower thanks to bitmask + precomputed closures.

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
} from "./thompson.js";
import { compileNfaSoa } from "./nfa-soa.js";
import { isPathSep } from "../options.js";
import { classMatches } from "../ast.js";
import { DirMatch } from "../dir-match.js";
import { computeStaticPrefixes } from "./ops.js";

function ctz32(v) {
  if (v === 0) return 32;
  let c = 0;
  if ((v & 0x0000ffff) === 0) {
    v >>>= 16;
    c += 16;
  }
  if ((v & 0x000000ff) === 0) {
    v >>>= 8;
    c += 8;
  }
  if ((v & 0x0000000f) === 0) {
    v >>>= 4;
    c += 4;
  }
  if ((v & 0x00000003) === 0) {
    v >>>= 2;
    c += 2;
  }
  if ((v & 0x00000001) === 0) {
    c += 1;
  }
  return c;
}

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
// Input is the SoA shape from `nfa-soa.js`: parallel arrays for tag,
// next-state, and split-second-branch. SPLIT/JUMP states are ε-only
// and walked transparently; everything else (byte-consumers, MATCH,
// DOT_GUARD) is a closure leaf and gets its bit set in `out`.
//
// Algorithm: memoized recursion over the ε-graph. Thompson's NFA
// guarantees the ε-graph is acyclic — every loop in the source
// pattern (Star, SepRun, OptSegmentsSlash, …) is broken by a
// byte-consumer state, which is a closure leaf. So each state's
// closure depends only on already-computed sub-closures, and we can
// fold them with a single OR into `out`. Visits each state exactly
// once: O(n × nWords) total instead of the per-state BFS that re-
// walks ε-paths n times.
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
    // bits region is `nWords` words; the `info` region is `n` words —
    // one packed word per state):
    //   [0,                       n*nWords)    closures (inner-loop hot)
    //   [n*nWords,            (n+1)*nWords)    initBits (read once/match)
    //   [(n+1)*nWords,        (n+2)*nWords)    acceptBits (read once/match)
    //   [(n+2)*nWords, (n+2)*nWords + n)       info (per state, hot loop)
    // `closures` retains its `[0..)` origin so the inner loop's
    // `closures[base + j]` access is unchanged; init/accept/info gain a
    // constant-offset add. `info[s]` becomes `combined[infoOff + s]`
    // — V8 hoists `infoOff` as a const across the inner loop, so the
    // extra add is folded into the addressing mode.
    //
    // Each state's `info` word packs:
    //   bits  0..3   tag      (T_MATCH..T_NULL = 0..9 fit in 4 bits)
    //   bits  4..7   flags    (bit 0 = dotProtected)
    //   bits  8..15  byte     (T_BYTE byte value)
    //   bits 16..31  next     (next-state index, ≤ 65535)
    // ε-only T_SPLIT / T_JUMP slots get tag = T_NULL — closures already
    // absorbed their edges so they're never consumed at byte-step.
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
    let hasDotGuard = false;
    for (let i = 0; i < n; i++) {
      const tag = tags[i];
      if (tag === T_SPLIT || tag === T_JUMP) {
        combined[infoOff + i] = T_NULL;
        continue;
      }
      combined[infoOff + i] = tag | (flagBits[i] << 4) | (byteVals[i] << 8) | (nexts[i] << 16);
      if (tag === T_DOT_GUARD) hasDotGuard = true;
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
    this.hasDotGuard = hasDotGuard;

    // Scratch buffers reused across match calls (single-threaded).
    // Pack `cur`, `nxt`, and (if dot-guard) `processed` into ONE
    // Uint32Array — one TypedArray wrapper instead of 2-3, saves
    // ~50 B per matcher in V8 wrapper overhead. Layout (nWords each):
    //   [0,         nWords)    cur
    //   [nWords,  2*nWords)    nxt
    //   [2*nWords,3*nWords)    processed (only if hasDotGuard)
    this._scratch = new Uint32Array(nWords * (hasDotGuard ? 3 : 2));

    // Lazy reach-to-accept (matchDir prefix mode). Computed on first
    // matchDir call from `closures` + `acceptBits`; never needs the
    // dropped ε-only states. The compile-time `nfa` SoA isn't retained
    // — its tags/nexts/splitsB drop out of scope after this constructor.
    this._reachToAccept = null;
  }

  static build(program, dot) {
    const nfa = compileNfaSoa(program, dot);
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

  isMatch(path) {
    if (!this.facts.accept(path)) return false;
    this._run(path);
    return this._isAccept(this._scratch);
  }

  matchDir(dirPath) {
    this._run(dirPath);
    return DirMatch.fromExactPrefix(this._isAccept(this._scratch), this._hasPrefixDescent());
  }

  _hasPrefixDescent() {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const nWords = this.nWords;
    const scratch = this._scratch;

    // The hypothetical separator step below consumes a `/`, which is never a
    // segment-start dot — so every `T_DOT_GUARD` in the live set always
    // passes here. Static closures stop AT a dot-guard (it is a byte-
    // conditional ε-leaf), so a separator consumer sitting behind one (e.g.
    // the `SEP` of `*/` under `dot=false`, whose live set is
    // `{ANY_NON_SEP, DOT_GUARD→SEP}`) is invisible to the raw `scratch` scan.
    // ε-expand the guards to a fixpoint first, or the subtree is wrongly
    // pruned. `_runFast`/`dot=true` programs have no guards, so `cur` just
    // equals the live set and this is a no-op.
    const cur = new Uint32Array(nWords);
    for (let w = 0; w < nWords; w++) cur[w] = scratch[w];
    let changed = true;
    while (changed) {
      changed = false;
      for (let w = 0; w < nWords; w++) {
        let word = cur[w];
        while (word !== 0) {
          const off = ctz32(word);
          const s = (w << 5) + off;
          word &= word - 1;
          const word2 = closures[infoOff + s];
          if ((word2 & 0xf) === T_DOT_GUARD) {
            const base = (word2 >>> 16) * nWords;
            for (let j = 0; j < nWords; j++) {
              const merged = cur[j] | closures[base + j];
              if (merged !== cur[j]) {
                cur[j] = merged;
                changed = true;
              }
            }
          }
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
    return this.hasDotGuard ? this._runDotGuard(path) : this._runFast(path);
  }

  _runFast(path) {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const clsRefs = this.clsRefs;
    const nWords = this.nWords;
    const scratch = this._scratch;
    // cur slice: scratch[0..nWords)   nxt slice: scratch[nWords..2*nWords)
    const nxtBase = nWords;
    const initOff = this.initOff;
    for (let w = 0; w < nWords; w++) scratch[w] = closures[initOff + w];

    let atSegStart = true;

    for (let p = 0; p < path.length; p++) {
      const c = path[p];
      const sep = isPathSep(c);
      // `info[s]` packs `dotProtected` at bit 4 (0x10). When the
      // current byte is a segment-start dot, set `dotMaskFlag = 0x10`
      // so the inner loop's `!(word2 & dotMaskFlag)` short-circuits
      // to `false` for dot-protected states. When it isn't, the flag
      // is `0` and the same expression is unconditionally `true` —
      // collapses the original `!((word2 & 0x10) && dotMask)` to one
      // bitwise AND + bool coerce per state-step.
      const dotMaskFlag = atSegStart && c === 0x2e ? 0x10 : 0;

      for (let w = 0; w < nWords; w++) scratch[nxtBase + w] = 0;

      for (let w = 0; w < nWords; w++) {
        let word = scratch[w];
        while (word !== 0) {
          const off = ctz32(word);
          const s = (w << 5) + off;
          word &= word - 1;
          const word2 = closures[infoOff + s];
          let matched = false;
          // Dense int `tag` (1..5 for byte-step states); `switch` lets
          // V8 generate a jump table cleaner than an if-else chain.
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

  _runDotGuard(path) {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const clsRefs = this.clsRefs;
    const nWords = this.nWords;
    const scratch = this._scratch;
    // cur: [0..nWords)   nxt: [nWords..2*nWords)   processed: [2*nWords..3*nWords)
    const nxtBase = nWords;
    const procBase = nWords * 2;
    const work = new Uint32Array(nWords);
    const initOff = this.initOff;
    for (let w = 0; w < nWords; w++) scratch[w] = closures[initOff + w];

    let atSegStart = true;

    for (let p = 0; p < path.length; p++) {
      const c = path[p];
      const sep = isPathSep(c);
      const dotMaskFlag = atSegStart && c === 0x2e ? 0x10 : 0;

      for (let w = 0; w < nWords; w++) {
        scratch[nxtBase + w] = 0;
        work[w] = scratch[w];
        scratch[procBase + w] = 0;
      }

      let anyWork = true;
      while (anyWork) {
        anyWork = false;
        for (let w = 0; w < nWords; w++) {
          let word = work[w];
          if (word === 0) continue;
          work[w] = 0;
          while (word !== 0) {
            const off = ctz32(word);
            const s = (w << 5) + off;
            const bit = 1 << off;
            word &= word - 1;
            if (scratch[procBase + w] & bit) continue;
            scratch[procBase + w] |= bit;
            const word2 = closures[infoOff + s];
            const tag = word2 & 0xf;
            if (tag === T_BYTE) {
              if (((word2 >>> 8) & 0xff) === c) {
                const base = (word2 >>> 16) * nWords;
                for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
              }
            } else if (tag === T_CLASS) {
              if (classMatches(clsRefs[s], c) && !(word2 & dotMaskFlag)) {
                const base = (word2 >>> 16) * nWords;
                for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
              }
            } else if (tag === T_ANY_NON_SEP) {
              if (!sep && !(word2 & dotMaskFlag)) {
                const base = (word2 >>> 16) * nWords;
                for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
              }
            } else if (tag === T_ANY_BYTE) {
              if (!(word2 & dotMaskFlag)) {
                const base = (word2 >>> 16) * nWords;
                for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
              }
            } else if (tag === T_SEP) {
              if (sep) {
                const base = (word2 >>> 16) * nWords;
                for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
              }
            } else if (tag === T_DOT_GUARD) {
              if (dotMaskFlag === 0) {
                const base = (word2 >>> 16) * nWords;
                for (let j = 0; j < nWords; j++) work[j] |= closures[base + j];
              }
            }
          }
          if (work[w] !== 0) anyWork = true;
        }
        // Re-scan if any new work appeared (DOT_GUARD chains).
        if (!anyWork) {
          for (let w = 0; w < nWords; w++) {
            if (work[w] !== 0) {
              anyWork = true;
              break;
            }
          }
        }
      }

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
