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
import { GlobError } from "../error.js";

// Reach-to-accept fixed-point over byte-consumer states. reach[s] = true
// iff from state s the matcher can reach the accept state via at least
// one byte step: (closure[S.next] ∩ acceptBits ≠ ∅) OR closure[S.next]
// contains any s' with reach[s']. T_DOT_GUARD also has a `next` and
// counts. T_SPLIT / T_JUMP slots are tagged T_NULL (closures absorbed
// their ε-edges) and skipped.
function reachFromClosures(closures, infoOff, acceptOff, nWords) {
  const n = closures.length - infoOff;
  const reach = new Uint8Array(n);
  // Reverse order follows the backward flow (Thompson allocates successors
  // after their predecessors), so this converges in ~2 passes. Order affects
  // speed only, the fixpoint is unique.
  let changed = true;
  while (changed) {
    changed = false;
    for (let s = n - 1; s >= 0; s--) {
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
// Thompson's NFA keeps the ε-graph acyclic, since every loop in the source
// pattern (Star, SepRun, OptSegmentsSlash, ..) is broken by a byte-consumer
// state that is a closure leaf. Each state's closure then depends only on
// already-computed sub-closures, folded with a single OR into `out`. Visits
// each state once, O(n × nWords) total.
//
// Post-order DFS on an explicit stack, mirroring the Rust twin: a recursive
// version overflowed the call stack on long forward ε-chains (thousands of
// `{,}` units). `~s` marks the exit phase (children done, fold closures).
function staticClosuresN(tags, nexts, splitsB, n, nWords, out) {
  const seen = new Uint8Array(n);
  const stack = [];
  for (let root = 0; root < n; root++) {
    if (seen[root]) continue;
    stack.push(root);
    while (stack.length > 0) {
      const item = stack.pop();
      if (item < 0) {
        const s = ~item;
        const tag = tags[s];
        const base = s * nWords;
        if (tag === T_SPLIT) {
          const aBase = nexts[s] * nWords;
          const bBase = splitsB[s] * nWords;
          for (let w = 0; w < nWords; w++) out[base + w] = out[aBase + w] | out[bBase + w];
        } else if (tag === T_JUMP) {
          const nxBase = nexts[s] * nWords;
          for (let w = 0; w < nWords; w++) out[base + w] = out[nxBase + w];
        } else {
          // Byte-consumer / DOT_GUARD / MATCH: closure(s) = {s}.
          out[base + (s >>> 5)] = 1 << (s & 31);
        }
        continue;
      }

      const s = item;
      if (seen[s]) continue;
      seen[s] = 1;
      // Push the exit marker before the children so it pops after them.
      stack.push(~s);
      const tag = tags[s];
      if (tag === T_SPLIT) {
        if (!seen[nexts[s]]) stack.push(nexts[s]);
        if (!seen[splitsB[s]]) stack.push(splitsB[s]);
      } else if (tag === T_JUMP) {
        if (!seen[nexts[s]]) stack.push(nexts[s]);
      }
    }
  }
}

export class PikeVm {
  constructor(nfa, facts, prefixes) {
    this.facts = facts;
    this.prefixes = prefixes;

    const n = nfa.n;
    // The info word packs `next` into 16 bits; a larger NFA would silently
    // truncate state ids and mis-match. Only near-64KB patterns can get
    // here. (The Rust twin's u32 state ids carry no such cap.)
    if (n > 0x10000) throw new GlobError("TooManyStates", { n, max: 0x10000 });
    const nWords = Math.max(1, (n + 31) >>> 5);
    this.nWords = nWords;

    // Co-allocate closures + acceptBits + per-state `info` into one
    // `Uint32Array`. Saves TypedArray wrappers + their backing-store
    // metadata vs separate arrays. The initial state's closure row doubles
    // as the init bits (`initOff` points into the closures region). Layout
    // (the acceptBits region is `nWords` words, the `info` region is `n`
    // words, one packed word per state):
    //   [0,               n*nWords)        closures (inner-loop hot)
    //   [n*nWords,    (n+1)*nWords)        acceptBits (read once/match)
    //   [(n+1)*nWords, (n+1)*nWords + n)   info (per state, hot loop)
    //
    // Each state's `info` word packs:
    //   bits  0..3   tag      (T_MATCH..T_NULL = 0..9 fit in 4 bits)
    //   bit   4      refuses a segment-start dot. Derived here from the
    //                state kind under dot=false: wildcards and negated
    //                classes refuse it, literals and positive classes
    //                match it explicitly (GLOB_SPEC §6).
    //   bits  8..15  byte     (T_BYTE byte value)
    //   bits 16..31  next     (next-state index, ≤ 65535)
    // ε-only T_SPLIT / T_JUMP slots get tag = T_NULL. Closures already
    // absorbed their edges, so they're never consumed at byte-step.
    // `clsRefs` stays an Object array (only ~10% of states use it, and
    // `cls` is a structured value we can't inline into bits).
    const acceptOff = n * nWords;
    const infoOff = acceptOff + nWords;
    const combined = new Uint32Array(infoOff + n);
    staticClosuresN(nfa.tags, nfa.nexts, nfa.splitsB, n, nWords, combined);
    const tags = nfa.tags;
    const nexts = nfa.nexts;
    const byteVals = nfa.byteVals;
    const inClsRefs = nfa.clsRefs;
    const protectFlag = nfa.dot ? 0 : 0x10;
    let clsRefs = null;
    const guardIds = [];
    for (let i = 0; i < n; i++) {
      const tag = tags[i];
      if (tag === T_SPLIT || tag === T_JUMP) {
        combined[infoOff + i] = T_NULL;
        continue;
      }
      let bits = tag | (byteVals[i] << 8) | (nexts[i] << 16);
      if (tag === T_ANY_NON_SEP || tag === T_ANY_BYTE || (tag === T_CLASS && inClsRefs[i].neg)) {
        bits |= protectFlag;
      }
      combined[infoOff + i] = bits;
      if (tag === T_DOT_GUARD) guardIds.push(i);
      if (tag === T_CLASS) {
        if (clsRefs === null) clsRefs = Array.from({ length: n });
        clsRefs[i] = inClsRefs[i];
      }
    }
    this.closures = combined;
    this.initOff = nfa.initial * nWords;
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
      // Fold guard chains until stable. Chains point forward (a guard's
      // expansion only ever holds later guards), so the reverse pass folds
      // each chain in one go and the loop converges in ~2 passes. Order
      // affects speed only, the fixpoint is unique.
      let changed = true;
      while (changed) {
        changed = false;
        for (let i = guardIds.length - 1; i >= 0; i--) {
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

    // acceptBits: the Match state, plus every guard whose transitive
    // expansion reaches it (a guard passes unconditionally at EOF — no next
    // byte can trip it). Everything else an active set can hold is a byte
    // consumer, which cannot accept without more input.
    const accept = nfa.accept;
    combined[acceptOff + (accept >>> 5)] |= 1 << (accept & 31);
    if (guardExps !== null) {
      const stride = 1 + nWords;
      for (let r = 0; r < guardExps.length; r += stride) {
        if (guardExps[r + 1 + (accept >>> 5)] & (1 << (accept & 31))) {
          const g = guardExps[r];
          combined[acceptOff + (g >>> 5)] |= 1 << (g & 31);
        }
      }
    }

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
    // Read the exact flag before the probe: `_hasPrefixDescent` reuses (and
    // overwrites) both scratch slots.
    const exact = this._isAccept(this._scratch);
    return DirMatch.fromExactPrefix(exact, this._hasPrefixDescent());
  }

  _hasPrefixDescent() {
    const closures = this.closures;
    const infoOff = this.infoOff;
    const nWords = this.nWords;
    const scratch = this._scratch;
    // Zero-alloc probe, mirroring the Rust twin: slot 0 (the final active
    // set, dead after matchDir's exact check) is expanded in place, and
    // slot 1 (stale `nxt` duplicate after `_run`) holds the post-`/` set.
    const nxtBase = nWords;

    // The separator step below consumes a `/`, never a segment-start dot, so
    // every `T_DOT_GUARD` in the live set passes here. Static closures stop at
    // a dot-guard (a byte-conditional ε-leaf), so a separator consumer behind
    // one (e.g. the `SEP` of `*/` under `dot=false`) would otherwise be
    // invisible to the raw `scratch` scan and the subtree wrongly pruned.
    const guardExps = this.guardExps;
    if (guardExps !== null) {
      const stride = 1 + nWords;
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
        const tag = word2 & 0xf;
        // Mirror the Rust byte_step for `/`: Sep and AnyByte always fire,
        // a Byte state holding a literal `/` (from single-branch brace
        // flattening, e.g. `x{a/b}y*`) fires too. Classes never match a
        // separator, either polarity.
        if (
          tag === T_SEP ||
          tag === T_ANY_BYTE ||
          (tag === T_BYTE && ((word2 >>> 8) & 0xff) === 0x2f)
        ) {
          const base = (word2 >>> 16) * nWords;
          for (let j = 0; j < nWords; j++) scratch[nxtBase + j] |= closures[base + j];
        }
      }
    }
    const acceptOff = this.acceptOff;
    for (let w = 0; w < nWords; w++) {
      if (scratch[nxtBase + w] & closures[acceptOff + w]) return true;
    }
    let reach = this._reachToAccept;
    if (reach === null) {
      reach = reachFromClosures(closures, infoOff, acceptOff, nWords);
      this._reachToAccept = reach;
    }
    for (let w = 0; w < nWords; w++) {
      let word = scratch[nxtBase + w];
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
      // `info[s]` bit 4 (0x10) marks states that refuse a segment-start
      // dot. At one, `dotMaskFlag = 0x10` so `!(word2 & dotMaskFlag)`
      // rejects them; otherwise it's `0` and always passes.
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
