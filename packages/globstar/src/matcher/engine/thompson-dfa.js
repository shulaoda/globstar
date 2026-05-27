// DFA via subset construction over a `Thompson` NFA. Same algorithm as
// the Rust crate's `engine/thompson_dfa.rs` — that side is the
// perf-tuned reference; this file tracks its semantics rather than its
// micro-optimizations. Runtime hot path is a pure typed-array lookup:
//
//   state = transitions[(state << strideShift) | classOfByte[byte]]
//
// Compile pipeline: `buildByteClasses` partitions bytes by the NFA
// states they fire (stride = nextPow2(numClasses)); `Builder` BFS-
// drains a work queue, interning each new active-set as a DFA state;
// `computeReachToAccept` lazily fills the prefix-mode bitmap on first
// `matchDir`. Builder uses two dedup paths: fast (NFA ≤ 64 states,
// (lo, hi) u32 pair + open-addressed hash) and wide (Uint32Array +
// string-keyed Map).

import {
  Thompson,
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
import { isPathSep } from "../options.js";
import { classMatches } from "../ast.js";
import { DirMatch } from "../dir-match.js";
import { computeStaticPrefixes } from "./ops.js";

const MAX_DFA_STATES = 4096;
const DEAD = 0;

export class ThompsonDfa {
  constructor(transitions, classOfByte, strideShift, accepting, reachToAccept, facts, prefixes) {
    this.transitions = transitions;
    this.classOfByte = classOfByte;
    this.strideShift = strideShift;
    this.accepting = accepting;
    this._reachToAccept = reachToAccept;
    this.facts = facts;
    this.prefixes = prefixes;
    this.initial = 1;
  }

  // Returns ThompsonDfa | null. null = state cap exceeded during BFS.
  static build(program, dot) {
    const thompson = Thompson.compile(program, dot);
    const { classOfByte, numClasses, tracksSegStart, hasDotGuard } = buildByteClasses(thompson);
    const strideShift = classStrideShift(numClasses);

    const builder = new Builder(
      thompson,
      classOfByte,
      numClasses,
      strideShift,
      tracksSegStart,
      hasDotGuard,
    );
    builder.seedDead();
    if (builder.internInitial() === null) return null;
    if (builder.run() === null) return null;

    // Copy realized state range into fresh buffers — `subarray` would
    // retain the whole over-allocated capacity via shared backing store.
    const n = builder.stateCount;
    const transitions = builder.transitions.slice(0, n << strideShift);
    const accepting = builder.accepting.slice(0, n);

    // `_reachToAccept` is only consulted by `matchDir` for prefix-mode
    // pruning. Defer the backward BFS to first matchDir call — saves
    // 3-5 µs from compile for `isMatch`-only users.
    return new ThompsonDfa(
      transitions,
      classOfByte,
      strideShift,
      accepting,
      null,
      program.facts,
      computeStaticPrefixes(program.ops),
    );
  }

  staticPrefixes() {
    return this.prefixes;
  }

  // `path` and `dirPath` are `Uint8Array` — encoded once by the public
  // `compileMatcher(...).match` / `matchDir` boundary via `pathBytes()`.
  isMatch(path) {
    if (!this.facts.accept(path)) return false;
    const trans = this.transitions;
    const cob = this.classOfByte;
    const shift = this.strideShift;
    let state = this.initial;
    for (let i = 0; i < path.length; i++) {
      state = trans[(state << shift) | cob[path[i]]];
    }
    return this.accepting[state] !== 0;
  }

  matchDir(dirPath) {
    const trans = this.transitions;
    const cob = this.classOfByte;
    const shift = this.strideShift;
    let state = this.initial;
    for (let i = 0; i < dirPath.length; i++) {
      state = trans[(state << shift) | cob[dirPath[i]]];
      if (state === DEAD) return DirMatch.Pruned;
    }
    const exact = this.accepting[state] !== 0;
    const sepCls = cob[0x2f];
    const afterSep = trans[(state << shift) | sepCls];
    if (afterSep === DEAD) return DirMatch.fromExactPrefix(exact, false);
    if (this.accepting[afterSep] !== 0) return DirMatch.fromExactPrefix(exact, true);
    let reach = this._reachToAccept;
    if (reach === null) {
      reach = computeReachToAccept(trans, shift, this.accepting);
      this._reachToAccept = reach;
    }
    return DirMatch.fromExactPrefix(exact, reach[afterSep] !== 0);
  }
}

// log2(nextPow2(max(2, n))). Lets `state * stride + class` collapse to
// `state << shift | class`.
function classStrideShift(numClasses) {
  let stride = 2;
  let shift = 1;
  while (stride < numClasses) {
    stride <<= 1;
    shift++;
  }
  return shift;
}

// Per-byte signature from category aggregates: T_BYTE → byByte[b];
// T_ANY_NON_SEP/T_SEP/T_ANY_BYTE → single aggregate sprayed into all
// non-sep/sep/every byte; T_CLASS → keyed by cls ref, one 256-sweep
// per unique class. Returns `{classOfByte, numClasses, tracksSegStart,
// hasDotGuard}`; `hasDotGuard` falls out of the same pass that derives
// `keySep`/`keyDot`.
function buildByteClasses(thompson) {
  const states = thompson.states;
  const n = states.length;

  // Single linear pass: detect (a) which key dimensions matter for
  // dedup and (b) whether step's dot-guard re-entry path is needed.
  let keySep = false;
  let keyDot = false;
  let hasDotGuard = false;
  for (let i = 0; i < n; i++) {
    const t = states[i];
    const tag = t.tag;
    if (tag === T_SEP) {
      keySep = true;
    } else if (tag === T_ANY_NON_SEP) {
      keySep = true;
      if (t.dotProtected) keyDot = true;
    } else if ((tag === T_ANY_BYTE || tag === T_CLASS) && t.dotProtected) {
      keyDot = true;
    } else if (tag === T_DOT_GUARD) {
      hasDotGuard = true;
    }
  }
  if (keyDot) keySep = true;

  const result =
    n <= 32
      ? buildByteClassesU32(states, n, keySep, keyDot)
      : buildByteClassesWide(states, n, keySep, keyDot);
  result.hasDotGuard = hasDotGuard;
  return result;
}

function buildByteClassesU32(states, n, keySep, keyDot) {
  // Aggregate state-masks per category.
  const byByte = new Uint32Array(256); // byte → bitmap of T_BYTE states
  const byClass = new Map(); // cls → bitmap of T_CLASS states
  let nonSepMask = 0;
  let sepMask = 0;
  let anyMask = 0;

  for (let s = 0; s < n; s++) {
    const t = states[s];
    const bit = 1 << s;
    switch (t.tag) {
      case T_BYTE:
        byByte[t.b] |= bit;
        break;
      case T_CLASS: {
        const prev = byClass.get(t.cls) || 0;
        byClass.set(t.cls, prev | bit);
        break;
      }
      case T_ANY_NON_SEP:
        nonSepMask |= bit;
        break;
      case T_SEP:
        sepMask |= bit;
        break;
      case T_ANY_BYTE:
        anyMask |= bit;
        break;
    }
  }

  // Dispatch into a class-aware or no-class specialization. Keeps each
  // sub-function monomorphic for V8 IC, and lets the no-class hot
  // path fuse signature compute + dedup in a single 256-byte sweep
  // (skipping the `signatures` Uint32Array allocation entirely).
  return byClass.size > 0
    ? _byteClassesU32WithClass(byByte, byClass, keySep, keyDot, anyMask, sepMask, nonSepMask)
    : _byteClassesU32NoClass(byByte, keySep, keyDot, anyMask, sepMask, nonSepMask);
}

function _byteClassesU32NoClass(byByte, keySep, keyDot, anyMask, sepMask, nonSepMask) {
  // Fused signature compute + dedup — `sig` is a per-iteration local.
  const classOfByte = new Uint8Array(256);
  const keyToClass = new Map();
  let numClasses = 0;
  for (let c = 0; c < 256; c++) {
    const isSep = isPathSep(c);
    let sig = byByte[c] | anyMask;
    if (isSep) sig |= sepMask;
    else sig |= nonSepMask;
    const flags = (keySep && isSep ? 2 : 0) | (keyDot && c === 0x2e ? 1 : 0);
    const key = sig * 4 + flags;
    let cls = keyToClass.get(key);
    if (cls === undefined) {
      cls = numClasses++;
      keyToClass.set(key, cls);
    }
    classOfByte[c] = cls;
  }
  return { classOfByte, numClasses, tracksSegStart: keyDot };
}

function _byteClassesU32WithClass(byByte, byClass, keySep, keyDot, anyMask, sepMask, nonSepMask) {
  const signatures = new Uint32Array(256);
  for (let c = 0; c < 256; c++) {
    let sig = byByte[c] | anyMask;
    if (isPathSep(c)) sig |= sepMask;
    else sig |= nonSepMask;
    signatures[c] = sig;
  }
  for (const [cls, mask] of byClass) {
    for (let c = 0; c < 256; c++) {
      if (classMatches(cls, c)) signatures[c] |= mask;
    }
  }
  const classOfByte = new Uint8Array(256);
  const keyToClass = new Map();
  let numClasses = 0;
  for (let c = 0; c < 256; c++) {
    const flags = (keySep && isPathSep(c) ? 2 : 0) | (keyDot && c === 0x2e ? 1 : 0);
    const key = signatures[c] * 4 + flags;
    let cls = keyToClass.get(key);
    if (cls === undefined) {
      cls = numClasses++;
      keyToClass.set(key, cls);
    }
    classOfByte[c] = cls;
  }
  return { classOfByte, numClasses, tracksSegStart: keyDot };
}

function buildByteClassesWide(states, n, keySep, keyDot) {
  const words = (n + 31) >>> 5;
  // Per-category aggregates as multi-word bitmaps.
  const byByte = new Uint32Array(256 * words);
  const byClass = new Map();
  const nonSepAgg = new Uint32Array(words);
  const sepAgg = new Uint32Array(words);
  const anyAgg = new Uint32Array(words);

  for (let s = 0; s < n; s++) {
    const t = states[s];
    const w = s >>> 5;
    const bit = 1 << (s & 31);
    switch (t.tag) {
      case T_BYTE:
        byByte[t.b * words + w] |= bit;
        break;
      case T_CLASS: {
        let arr = byClass.get(t.cls);
        if (arr === undefined) {
          arr = new Uint32Array(words);
          byClass.set(t.cls, arr);
        }
        arr[w] |= bit;
        break;
      }
      case T_ANY_NON_SEP:
        nonSepAgg[w] |= bit;
        break;
      case T_SEP:
        sepAgg[w] |= bit;
        break;
      case T_ANY_BYTE:
        anyAgg[w] |= bit;
        break;
    }
  }

  const signatures = new Uint32Array(256 * words);
  for (let c = 0; c < 256; c++) {
    const off = c * words;
    const cat = isPathSep(c) ? sepAgg : nonSepAgg;
    for (let w = 0; w < words; w++) {
      signatures[off + w] = byByte[off + w] | anyAgg[w] | cat[w];
    }
  }
  for (const [cls, mask] of byClass) {
    for (let c = 0; c < 256; c++) {
      if (!classMatches(cls, c)) continue;
      const off = c * words;
      for (let w = 0; w < words; w++) signatures[off + w] |= mask[w];
    }
  }

  // Pack each per-byte signature word into 2 char codes + a final
  // flags char. `String.fromCharCode.apply` is much cheaper than
  // `toString(36) + ","` per word (no per-char base-36 conversion).
  const classOfByte = new Uint8Array(256);
  const keyToClass = new Map();
  const codes = Array.from({ length: words * 2 + 1 });
  let numClasses = 0;
  for (let c = 0; c < 256; c++) {
    const off = c * words;
    for (let w = 0; w < words; w++) {
      const v = signatures[off + w];
      codes[w * 2] = v & 0xffff;
      codes[w * 2 + 1] = (v >>> 16) & 0xffff;
    }
    codes[words * 2] = (keySep && isPathSep(c) ? 2 : 0) | (keyDot && c === 0x2e ? 1 : 0);
    const key = String.fromCharCode.apply(null, codes);
    let cls = keyToClass.get(key);
    if (cls === undefined) {
      cls = numClasses++;
      keyToClass.set(key, cls);
    }
    classOfByte[c] = cls;
  }
  return { classOfByte, numClasses, tracksSegStart: keyDot };
}

// Per-NFA-state ε-closure as (lo, hi) bitmap pair (≤ 64 NFA states).
// closure(s) = byte-consumer / Match / DotGuard leaves reachable from
// s via Split/Jump. DotGuard is a leaf — `_stepFast` re-injects
// closure(t.next) into its work queue iff the guard passes (byte-
// conditional, not statically bakeable). Generation-based `visited`
// avoids per-iteration `fill(0)`.
function computeStaticClosuresU32(thompson) {
  const states = thompson.states;
  const n = states.length;
  const lo = new Uint32Array(n);
  const hi = new Uint32Array(n);
  // Generation-based visited table — bumping `gen` invalidates all
  // entries in O(1), avoiding a `visited.fill(0)` per outer iteration.
  const visited = new Uint32Array(n);
  const stack = [];
  let gen = 0;

  for (let s = 0; s < n; s++) {
    gen++;
    if (gen === 0) {
      visited.fill(0);
      gen = 1;
    }
    stack.length = 0;
    stack.push(s);
    visited[s] = gen;
    while (stack.length > 0) {
      const cur = stack.pop();
      const t = states[cur];
      const tag = t.tag;
      if (tag === T_SPLIT) {
        if (visited[t.a] !== gen) {
          visited[t.a] = gen;
          stack.push(t.a);
        }
        if (visited[t.splitB] !== gen) {
          visited[t.splitB] = gen;
          stack.push(t.splitB);
        }
      } else if (tag === T_JUMP) {
        if (visited[t.next] !== gen) {
          visited[t.next] = gen;
          stack.push(t.next);
        }
      } else {
        // Leaf: T_BYTE / T_CLASS / T_ANY_NON_SEP / T_ANY_BYTE / T_SEP /
        // T_DOT_GUARD / T_MATCH.
        if (cur < 32) lo[s] |= 1 << cur;
        else hi[s] |= 1 << (cur - 32);
      }
    }
  }
  return { lo, hi };
}

function ctz32(v) {
  let c = 0;
  if ((v & 0xffff) === 0) {
    v >>>= 16;
    c += 16;
  }
  if ((v & 0xff) === 0) {
    v >>>= 8;
    c += 8;
  }
  if ((v & 0xf) === 0) {
    v >>>= 4;
    c += 4;
  }
  if ((v & 0x3) === 0) {
    v >>>= 2;
    c += 2;
  }
  if ((v & 0x1) === 0) c += 1;
  return c;
}

// FxHash-flavored 32-bit mix on (lo, hi, flag).
function hashKey(lo, hi, flag) {
  let h = (0 ^ lo) | 0;
  h = Math.imul(h, 0x9e3779b1);
  h = (h ^ hi) | 0;
  h = Math.imul(h, 0x85ebca77);
  h = (h ^ flag) | 0;
  h = Math.imul(h, 0xc2b2ae3d);
  h ^= h >>> 16;
  return h | 0;
}

const DEDUP_INITIAL_CAP = 16;
const DEDUP_MAX_CAP = 8192;
const EMPTY_U32 = new Uint32Array(0);

class Builder {
  constructor(thompson, classOfByte, numClasses, strideShift, tracksSegStart, hasDotGuard) {
    this.thompson = thompson;
    this.classOfByte = classOfByte;
    this.numClasses = numClasses;
    this.strideShift = strideShift;
    this.tracksSegStart = tracksSegStart;
    this.hasDotGuard = hasDotGuard;

    const n = thompson.states.length;
    this.useFastDedup = n <= 64;

    // Per-DFA-state metadata (parallel arrays):
    //   stateBitsLo/Hi[i]   — active NFA-state bitmap (fast path)
    //   stateNfa[i]         — sorted Uint32Array (wide path; null on fast)
    //   stateAtSegStart[i]  — segment-start flag (0/1)
    this.stateBitsLo = [];
    this.stateBitsHi = [];
    this.stateAtSegStart = [];
    this.stateNfa = this.useFastDedup ? null : [];

    // Initial-state shortcut keys (fast path); set by `internInitial`.
    this.initLo = 0;
    this.initHi = 0;
    this.initFlag = 0;
    this.initId = -1;

    // Path-specific build state — see `_initFastDedup` / `_initWide`.
    if (this.useFastDedup) this._initFastDedup(thompson, n);
    else this._initWide(n);

    // Transitions / accepting tables. Grow geometrically; auto-zeroed
    // on alloc means new state rows start at DEAD without manual fill.
    const initialCap = 4;
    this.transitions = new Uint16Array(initialCap << strideShift);
    this.accepting = new Uint8Array(initialCap);
    this.transCap = initialCap;
    this.stateCount = 0;
    this.queue = [];
  }

  // Fast-dedup setup (NFA ≤ 64 states): static ε-closure bitmaps,
  // accepts-at-EOF bitmap, open-addressed (lo, hi, flag) hash table.
  // No per-step ε-closure scratch needed — closures are baked at
  // compile time.
  _initFastDedup(thompson, n) {
    const cl = computeStaticClosuresU32(thompson);
    this.closureLo = cl.lo;
    this.closureHi = cl.hi;

    // accepts-at-EOF as a (lo, hi) bitmap so the per-step accept
    // check is one AND.
    let aLo = 0;
    let aHi = 0;
    const eof = thompson.acceptsAtEof;
    for (let i = 0; i < n; i++) {
      if (!eof[i]) continue;
      if (i < 32) aLo |= 1 << i;
      else aHi |= 1 << (i - 32);
    }
    this.acceptsLo = aLo;
    this.acceptsHi = aHi;

    this.dedupSlots = new Int32Array(DEDUP_INITIAL_CAP).fill(-1);
    this.dedupKeyLo = new Int32Array(DEDUP_INITIAL_CAP);
    this.dedupKeyHi = new Int32Array(DEDUP_INITIAL_CAP);
    this.dedupKeyFlag = new Uint8Array(DEDUP_INITIAL_CAP);
    this.dedupCap = DEDUP_INITIAL_CAP;
    this.dedupMask = DEDUP_INITIAL_CAP - 1;
    this.dedupCount = 0;
  }

  // Wide-path setup (NFA > 64 states): string-keyed Map dedup plus
  // ε-closure scratch buffers (the wide path keeps the dynamic BFS
  // since static closure bitmaps wouldn't fit in u64).
  _initWide(n) {
    this.fallbackMap = new Map();
    this.keyWordsPerSet = (n + 31) >>> 5;
    this.fallbackKeyWords = new Uint32Array(this.keyWordsPerSet);
    this.fallbackKeyCodes = Array.from({ length: this.keyWordsPerSet * 2 + 1 });

    this.scratchSuccessors = [];
    this.closureVisited = new Uint32Array(n);
    this.closureGeneration = 0;
    this.closureStack = [];
  }

  seedDead() {
    if (this.useFastDedup) {
      // DEAD has implicit key (0, 0, 0 → id 0). step()'s fast path
      // handles it inline, no dedup-table entry needed.
      this._allocStateFast(0, 0, 0, 0);
    } else {
      this.fallbackMap.set(stateKeyFallback(EMPTY_U32, 0, this), DEAD);
      this._allocStateWide(EMPTY_U32, 0, 0);
    }
  }

  internInitial() {
    const atSegStart = this.tracksSegStart ? 1 : 0;
    const initial = this.thompson.initial;

    if (this.useFastDedup) {
      // Bootstrap from precomputed closure of the initial NFA state.
      const lo = this.closureLo[initial];
      const hi = this.closureHi[initial];
      const acceptsEmpty = ((lo & this.acceptsLo) | (hi & this.acceptsHi)) !== 0 ? 1 : 0;
      const id = this._allocStateFast(lo, hi, atSegStart, acceptsEmpty);
      this.initLo = lo;
      this.initHi = hi;
      this.initFlag = atSegStart;
      this.initId = id;
      this._dedupInsert(lo, hi, atSegStart, id);
      this.queue.push(id);
      return id;
    }

    // Wide path: dynamic ε-closure (>64 NFA states).
    const scratch = this.scratchSuccessors;
    scratch.length = 0;
    scratch.push(initial);
    epsilonClosureInPlace(this.thompson, scratch, this);
    const eof = this.thompson.acceptsAtEof;
    let acceptsEmpty = 0;
    for (let i = 0; i < scratch.length; i++) {
      if (eof[scratch[i]]) {
        acceptsEmpty = 1;
        break;
      }
    }
    scratch.sort((a, b) => a - b);
    const nfaCopy = Uint32Array.from(scratch);
    const id = this._allocStateWide(nfaCopy, atSegStart, acceptsEmpty);
    this.fallbackMap.set(stateKeyFallback(nfaCopy, atSegStart, this), id);
    this.queue.push(id);
    return id;
  }

  _ensureCapacity(stateCount) {
    if (stateCount <= this.transCap) return;
    let newCap = this.transCap * 2;
    while (newCap < stateCount) newCap *= 2;
    const newTrans = new Uint16Array(newCap << this.strideShift);
    newTrans.set(this.transitions);
    this.transitions = newTrans;
    const newAcc = new Uint8Array(newCap);
    newAcc.set(this.accepting);
    this.accepting = newAcc;
    this.transCap = newCap;
  }

  _allocStateFast(lo, hi, atSegStart, accepts) {
    const id = this.stateCount;
    this._ensureCapacity(id + 1);
    this.stateBitsLo.push(lo);
    this.stateBitsHi.push(hi);
    this.stateAtSegStart.push(atSegStart);
    this.accepting[id] = accepts;
    this.stateCount = id + 1;
    return id;
  }

  _allocStateWide(nfaArr, atSegStart, accepts) {
    const id = this.stateCount;
    this._ensureCapacity(id + 1);
    this.stateBitsLo.push(0);
    this.stateBitsHi.push(0);
    this.stateNfa.push(nfaArr);
    this.stateAtSegStart.push(atSegStart);
    this.accepting[id] = accepts;
    this.stateCount = id + 1;
    return id;
  }

  // BFS: drain the queue, computing one successor per byte class per
  // pending state. Returns null on state-cap overflow.
  run() {
    const repByte = new Uint8Array(this.numClasses);
    for (let c = 0; c < 256; c++) repByte[this.classOfByte[c]] = c;

    // Bind once outside the inner loop so V8 keeps it monomorphic.
    const stepFn = !this.useFastDedup
      ? this._stepWide.bind(this)
      : this.hasDotGuard
        ? this._stepFastDotGuard.bind(this)
        : this._stepFastNoDotGuard.bind(this);

    while (this.queue.length > 0) {
      const id = this.queue.pop();
      const base = id << this.strideShift;
      for (let cls = 0; cls < this.numClasses; cls++) {
        const nextId = stepFn(id, repByte[cls]);
        if (nextId === null) return null;
        this.transitions[base | cls] = nextId;
      }
    }
    return true;
  }

  // Fast path for dot=true patterns (no T_DOT_GUARD in NFA, so no
  // ε-pass re-entry into the work queue). One pass over lo then hi,
  // no `processed` dedup, no outer while.
  _stepFastNoDotGuard(fromId, byte) {
    const atSegStart = this.stateAtSegStart[fromId] !== 0;
    const states = this.thompson.states;
    const closureLo = this.closureLo;
    const closureHi = this.closureHi;
    const sep = isPathSep(byte);
    const dotMask = atSegStart && byte === 0x2e;

    let outLo = 0;
    let outHi = 0;

    let lo = this.stateBitsLo[fromId];
    while (lo !== 0) {
      const s = ctz32(lo);
      lo &= lo - 1;
      const t = states[s];
      const tag = t.tag;
      if (tag === T_BYTE) {
        if (t.b === byte) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_CLASS) {
        if (classMatches(t.cls, byte) && !(t.dotProtected && dotMask)) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_ANY_NON_SEP) {
        if (!sep && !(t.dotProtected && dotMask)) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_ANY_BYTE) {
        if (!(t.dotProtected && dotMask)) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_SEP) {
        if (sep) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      }
      // T_MATCH: terminal, no transition. T_DOT_GUARD: cannot occur
      // (`hasDotGuard` is false on this dispatch path).
    }
    let hi = this.stateBitsHi[fromId];
    while (hi !== 0) {
      const s = 32 + ctz32(hi);
      hi = (hi & (hi - 1)) >>> 0;
      const t = states[s];
      const tag = t.tag;
      if (tag === T_BYTE) {
        if (t.b === byte) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_CLASS) {
        if (classMatches(t.cls, byte) && !(t.dotProtected && dotMask)) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_ANY_NON_SEP) {
        if (!sep && !(t.dotProtected && dotMask)) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_ANY_BYTE) {
        if (!(t.dotProtected && dotMask)) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      } else if (tag === T_SEP) {
        if (sep) {
          outLo |= closureLo[t.next];
          outHi |= closureHi[t.next];
        }
      }
    }

    return this._internStep(outLo, outHi, this.tracksSegStart && sep ? 1 : 0);
  }

  // Full path for dot=false patterns whose NFA contains T_DOT_GUARD —
  // its ε-edge is byte-conditional, so the work queue can re-fill and
  // we must dedup with `processedLo/Hi`.
  _stepFastDotGuard(fromId, byte) {
    const atSegStart = this.stateAtSegStart[fromId] !== 0;
    const states = this.thompson.states;
    const closureLo = this.closureLo;
    const closureHi = this.closureHi;
    const sep = isPathSep(byte);
    const dotMask = atSegStart && byte === 0x2e;

    let outLo = 0;
    let outHi = 0;
    let workLo = this.stateBitsLo[fromId];
    let workHi = this.stateBitsHi[fromId];
    let processedLo = 0;
    let processedHi = 0;
    while (workLo !== 0 || workHi !== 0) {
      while (workLo !== 0) {
        const s = ctz32(workLo);
        const bit = 1 << s;
        workLo &= workLo - 1;
        if (processedLo & bit) continue;
        processedLo |= bit;
        const t = states[s];
        const tag = t.tag;
        if (tag === T_BYTE) {
          if (t.b === byte) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_CLASS) {
          if (classMatches(t.cls, byte) && !(t.dotProtected && dotMask)) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_ANY_NON_SEP) {
          if (!sep && !(t.dotProtected && dotMask)) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_ANY_BYTE) {
          if (!(t.dotProtected && dotMask)) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_SEP) {
          if (sep) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_DOT_GUARD) {
          if (!dotMask) {
            workLo |= closureLo[t.next];
            workHi |= closureHi[t.next];
          }
        }
      }
      while (workHi !== 0) {
        const off = ctz32(workHi);
        const s = 32 + off;
        const bit = 1 << off;
        workHi = (workHi & (workHi - 1)) >>> 0;
        if (processedHi & bit) continue;
        processedHi |= bit;
        const t = states[s];
        const tag = t.tag;
        if (tag === T_BYTE) {
          if (t.b === byte) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_CLASS) {
          if (classMatches(t.cls, byte) && !(t.dotProtected && dotMask)) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_ANY_NON_SEP) {
          if (!sep && !(t.dotProtected && dotMask)) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_ANY_BYTE) {
          if (!(t.dotProtected && dotMask)) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_SEP) {
          if (sep) {
            outLo |= closureLo[t.next];
            outHi |= closureHi[t.next];
          }
        } else if (tag === T_DOT_GUARD) {
          if (!dotMask) {
            workLo |= closureLo[t.next];
            workHi |= closureHi[t.next];
          }
        }
      }
    }

    return this._internStep(outLo, outHi, this.tracksSegStart && sep ? 1 : 0);
  }

  // Shared post-step intern: well-known fast paths, dedup probe,
  // insert (with bitmap-AND accept check).
  _internStep(outLo, outHi, nextSegStart) {
    if (outLo === 0 && outHi === 0 && nextSegStart === 0) return DEAD;
    if (outLo === this.initLo && outHi === this.initHi && nextSegStart === this.initFlag) {
      return this.initId;
    }

    const mask = this.dedupMask;
    const slots = this.dedupSlots;
    const keyLo = this.dedupKeyLo;
    const keyHi = this.dedupKeyHi;
    const keyFlag = this.dedupKeyFlag;
    let slot = hashKey(outLo, outHi, nextSegStart) & mask;
    while (true) {
      const id = slots[slot];
      if (id === -1) break;
      if (keyLo[slot] === outLo && keyHi[slot] === outHi && keyFlag[slot] === nextSegStart) {
        return id;
      }
      slot = (slot + 1) & mask;
    }

    if (this.stateCount >= MAX_DFA_STATES) return null;
    const accepts = ((outLo & this.acceptsLo) | (outHi & this.acceptsHi)) !== 0 ? 1 : 0;
    const newId = this._allocStateFast(outLo, outHi, nextSegStart, accepts);
    this._ensureDedupCapacity();
    this._dedupInsert(outLo, outHi, nextSegStart, newId);
    this.queue.push(newId);
    return newId;
  }

  _stepWide(fromId, byte) {
    const fromNfa = this.stateNfa[fromId];
    const atSegStart = this.stateAtSegStart[fromId] !== 0;
    const scratch = this.scratchSuccessors;
    scratch.length = 0;
    for (let i = 0; i < fromNfa.length; i++) {
      collectSuccessors(this.thompson, fromNfa[i], byte, atSegStart, scratch);
    }
    if (scratch.length === 0) return DEAD;
    epsilonClosureInPlace(this.thompson, scratch, this);
    scratch.sort((a, b) => a - b);

    const nextSegStart = this.tracksSegStart && isPathSep(byte) ? 1 : 0;
    const key = stateKeyFallback(scratch, nextSegStart, this);
    const existing = this.fallbackMap.get(key);
    if (existing !== undefined) return existing;
    if (this.stateCount >= MAX_DFA_STATES) return null;

    const nfaCopy = Uint32Array.from(scratch);
    const eof = this.thompson.acceptsAtEof;
    let accepts = 0;
    for (let i = 0; i < scratch.length; i++) {
      if (eof[scratch[i]]) {
        accepts = 1;
        break;
      }
    }
    const newId = this._allocStateWide(nfaCopy, nextSegStart, accepts);
    this.fallbackMap.set(key, newId);
    this.queue.push(newId);
    return newId;
  }

  _dedupInsert(lo, hi, flag, id) {
    let slot = hashKey(lo, hi, flag) & this.dedupMask;
    while (this.dedupSlots[slot] !== -1) slot = (slot + 1) & this.dedupMask;
    this.dedupSlots[slot] = id;
    this.dedupKeyLo[slot] = lo;
    this.dedupKeyHi[slot] = hi;
    this.dedupKeyFlag[slot] = flag;
    this.dedupCount++;
  }

  _ensureDedupCapacity() {
    if (this.dedupCount * 2 < this.dedupCap || this.dedupCap >= DEDUP_MAX_CAP) return;
    const newCap = this.dedupCap * 2;
    const newMask = newCap - 1;
    const newSlots = new Int32Array(newCap).fill(-1);
    const newLo = new Int32Array(newCap);
    const newHi = new Int32Array(newCap);
    const newFlag = new Uint8Array(newCap);
    const oldSlots = this.dedupSlots;
    const oldLo = this.dedupKeyLo;
    const oldHi = this.dedupKeyHi;
    const oldFlag = this.dedupKeyFlag;
    for (let i = 0; i < this.dedupCap; i++) {
      const id = oldSlots[i];
      if (id === -1) continue;
      const lo = oldLo[i];
      const hi = oldHi[i];
      const flag = oldFlag[i];
      let probe = hashKey(lo, hi, flag) & newMask;
      while (newSlots[probe] !== -1) probe = (probe + 1) & newMask;
      newSlots[probe] = id;
      newLo[probe] = lo;
      newHi[probe] = hi;
      newFlag[probe] = flag;
    }
    this.dedupSlots = newSlots;
    this.dedupKeyLo = newLo;
    this.dedupKeyHi = newHi;
    this.dedupKeyFlag = newFlag;
    this.dedupCap = newCap;
    this.dedupMask = newMask;
  }
}

// Wide-NFA fallback key: pack the active-set bitmap into a string of
// fixed length for use as a `Map` key.
function stateKeyFallback(activeSet, atSegStart, builder) {
  const buf = builder.fallbackKeyWords;
  const wordsPerSet = builder.keyWordsPerSet;
  for (let i = 0; i < wordsPerSet; i++) buf[i] = 0;
  for (let i = 0; i < activeSet.length; i++) {
    const s = activeSet[i];
    buf[s >>> 5] |= 1 << (s & 31);
  }
  const codes = builder.fallbackKeyCodes;
  for (let i = 0; i < wordsPerSet; i++) {
    const w = buf[i];
    codes[i * 2] = w & 0xffff;
    codes[i * 2 + 1] = (w >>> 16) & 0xffff;
  }
  codes[wordsPerSet * 2] = atSegStart;
  return String.fromCharCode.apply(null, codes);
}

// ε-closure expansion in place. After this call `states` is the set of
// leaf NFA states (byte-consumers + DotGuard + Match) reachable from
// the input by traversing only Split/Jump edges.
function epsilonClosureInPlace(thompson, states, builder) {
  builder.closureGeneration += 1;
  if (builder.closureGeneration === 0) {
    builder.closureVisited.fill(0);
    builder.closureGeneration = 1;
  }
  const gen = builder.closureGeneration;
  const visited = builder.closureVisited;
  const stack = builder.closureStack;

  stack.length = 0;
  for (let i = 0; i < states.length; i++) {
    const s = states[i];
    if (visited[s] !== gen) {
      visited[s] = gen;
      stack.push(s);
    }
  }
  states.length = 0;
  const tStates = thompson.states;
  while (stack.length > 0) {
    const s = stack.pop();
    const t = tStates[s];
    const tag = t.tag;
    if (tag === T_SPLIT) {
      const a = t.a;
      const b = t.splitB;
      if (visited[a] !== gen) {
        visited[a] = gen;
        stack.push(a);
      }
      if (visited[b] !== gen) {
        visited[b] = gen;
        stack.push(b);
      }
    } else if (tag === T_JUMP) {
      const n = t.next;
      if (visited[n] !== gen) {
        visited[n] = gen;
        stack.push(n);
      }
    } else {
      states.push(s);
    }
  }
}

// Apply byte `c` from NFA state `s`, depositing successors (with
// ε-closure resolution for DotGuard / Split / Jump) into `out`.
function collectSuccessors(thompson, s, c, atSegStart, out) {
  const t = thompson.states[s];
  switch (t.tag) {
    case T_BYTE:
      if (t.b === c) out.push(t.next);
      return;
    case T_CLASS:
      if (classMatches(t.cls, c) && !(t.dotProtected && atSegStart && c === 0x2e)) {
        out.push(t.next);
      }
      return;
    case T_ANY_NON_SEP:
      if (!isPathSep(c) && !(t.dotProtected && atSegStart && c === 0x2e)) {
        out.push(t.next);
      }
      return;
    case T_ANY_BYTE:
      if (!(t.dotProtected && atSegStart && c === 0x2e)) out.push(t.next);
      return;
    case T_SEP:
      if (isPathSep(c)) out.push(t.next);
      return;
    case T_DOT_GUARD:
      if (!(atSegStart && c === 0x2e)) {
        collectSuccessors(thompson, t.next, c, atSegStart, out);
      }
      return;
    case T_SPLIT:
      collectSuccessors(thompson, t.a, c, atSegStart, out);
      collectSuccessors(thompson, t.splitB, c, atSegStart, out);
      return;
    case T_JUMP:
      collectSuccessors(thompson, t.next, c, atSegStart, out);
      return;
    case T_MATCH:
      return;
  }
}

// Backward BFS: mark every DFA state that can reach a non-self
// accepting state via one or more byte transitions.
function computeReachToAccept(transitions, strideShift, accepting) {
  const n = accepting.length;
  const stride = 1 << strideShift;
  const rev = Array.from({ length: n });
  for (let i = 0; i < n; i++) rev[i] = [];

  for (let from = 0; from < n; from++) {
    const base = from << strideShift;
    for (let cls = 0; cls < stride; cls++) {
      const to = transitions[base | cls];
      if (to === DEAD) continue;
      rev[to].push(from);
    }
  }

  const reach = new Uint8Array(n);
  const stack = [];
  for (let i = 0; i < n; i++) {
    if (accepting[i]) {
      const preds = rev[i];
      for (let j = 0; j < preds.length; j++) {
        const prev = preds[j];
        if (!reach[prev]) {
          reach[prev] = 1;
          stack.push(prev);
        }
      }
    }
  }
  while (stack.length > 0) {
    const s = stack.pop();
    const preds = rev[s];
    for (let j = 0; j < preds.length; j++) {
      const prev = preds[j];
      if (!reach[prev]) {
        reach[prev] = 1;
        stack.push(prev);
      }
    }
  }
  return reach;
}
