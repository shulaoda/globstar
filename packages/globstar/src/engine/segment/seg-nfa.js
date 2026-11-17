import { N_ANYCHAR, N_BRACE, N_LITERAL, N_STAR, seq, N_CLASS, lit } from "../../parse/ast.js";
import { ctz32 } from "../bytes.js";
import { isAscii, utf8Latin1 } from "../../utf8.js";
import { exact, EL_JOIN, EL_LIT, EL_PIECE, EL_WILD, INNER, ACCEPT } from "./graph.js";
import { byteSet, setEmpty, setHas, wordsFor, bitTest, bitSet, bitsIsZero } from "./bits.js";
import { NEVER_LEN } from "./wild.js";

const S_BYTE = 0;
const S_CLASS = 1;
const S_ANY = 2;
const S_SPLIT = 3;
const S_EXIT = 4;

const UNSET = -1;

const MAX_CLOSURE_BYTES = 128 * 1024;

const LENS_SCRATCH = 64;
const LENS_LO = new Int32Array(LENS_SCRATCH);
const LENS_HI = new Int32Array(LENS_SCRATCH);
const LENS_TAIL = new Float64Array(LENS_SCRATCH);

export function compileSegNfa(nodes, dot, ci) {
  const builder = new SegBuilder(ci);
  const entry = builder.compileNodes(nodes);
  const accept = builder.alloc(S_EXIT, 0, UNSET);
  for (const t of builder.tails) builder.patch(t, accept);
  const small = builder.kinds.length <= 32;
  const nfa = small ? new SegNfaSmall(builder, entry, dot) : new SegNfaWide(builder, entry, dot);
  const test = small ? (a, s) => (a & (1 << s)) !== 0 : bitTest;
  const good = new Uint8Array(builder.kinds.length);
  reach(builder, entry, good.length, true, good);
  return {
    matcher: nfa,
    minLen: good[entry] ? 0 : NEVER_LEN,
    dotProtect: !dot && !takesLitDot(builder, nfa.initDotBlocked, test),
    anySeg: anySeg(builder, entry, nfa.init, test),
  };
}

function letterClass(b) {
  const lower = b | 0x20;
  if (lower < 0x61 || lower > 0x7a) return null;
  const bits = [0, 0, 0, 0, 0, 0, 0, 0];
  for (const c of [lower - 0x20, lower]) bits[c >> 5] |= 1 << (c & 31);
  return { neg: false, bits };
}

function takesLitDot(builder, initDotBlocked, test) {
  const n = builder.kinds.length;
  for (let s = 0; s < n; s++) {
    if (!test(initDotBlocked, s)) continue;
    if (builder.kinds[s] === S_BYTE && builder.byteVals[s] === 0x2e) return true;
    if (builder.kinds[s] === S_CLASS && setHas(builder.clsRefs[s], 0x2e)) return true;
  }
  return false;
}

function reach(nfa, lo, hi, exitOn, good) {
  const { kinds, nexts, splitBs, clsRefs } = nfa;
  const to = (s, t) => t !== UNSET && t !== s && good[t] === 1;
  for (let s = hi - 1; s >= lo; s--) {
    const k = kinds[s];
    let on;
    if (k === S_EXIT) on = exitOn;
    else if (k === S_CLASS && setEmpty(clsRefs[s])) on = false;
    else on = to(s, nexts[s]) || to(s, splitBs[s]);
    if (on) good[s] = 1;
  }
}

function closeSmall(nfa, set, block) {
  const one = Int32Array.of(set);
  closeWide(nfa, one, block);
  return one[0];
}

function closeWide(nfa, set, block) {
  const kinds = nfa.kinds;
  const nexts = nfa.nexts;
  const splitBs = nfa.splitBs;
  for (let w = 0; w < set.length; w++) {
    let word = set[w];
    let todo = word;
    while (todo !== 0) {
      const bit = todo & -todo;
      todo ^= bit;
      const s = w * 32 + ctz32(bit);
      const k = kinds[s];
      let a = -1;
      let b = -1;
      if (k === S_SPLIT) {
        a = nexts[s];
        b = splitBs[s];
        word ^= bit;
      } else if (k === S_ANY) {
        if (block) word ^= bit;
        else if (nexts[s] === s) a = splitBs[s];
      } else if (k === S_CLASS && block && nfa.clsRefs[s].neg) {
        word ^= bit;
      } else continue;
      if (a !== -1) {
        if (a >>> 5 === w) {
          word |= 1 << (a & 31);
          todo |= 1 << (a & 31);
        } else set[a >>> 5] |= 1 << (a & 31);
      }
      if (b !== -1) {
        if (b >>> 5 === w) {
          word |= 1 << (b & 31);
          todo |= 1 << (b & 31);
        } else set[b >>> 5] |= 1 << (b & 31);
      }
    }
    set[w] = word;
  }
}

class SegNfaSmall {
  constructor(builder, entry, dot) {
    this.kinds = exact(builder.kinds);
    this.byteVals = exact(builder.byteVals);
    this.nexts = exact(builder.nexts);
    this.clsRefs = exact(builder.clsRefs);
    const n = builder.kinds.length;
    const closures = [];
    for (let s = 0; s < n; s++) closures.push(0);
    for (let s = n - 1; s >= 0; s--) {
      const k = builder.kinds[s];
      if (k === S_SPLIT) {
        const b = builder.splitBs[s];
        closures[s] = closures[builder.nexts[s]] | (b === UNSET ? 0 : closures[b]);
      } else if (k === S_ANY && builder.nexts[s] === s) {
        closures[s] = (1 << s) | closures[builder.splitBs[s]];
      } else closures[s] = 1 << s;
    }
    this.closures = exact(closures);
    this.init = closures[entry];
    this.initDotBlocked = dot ? this.init : closeSmall(builder, 1 << entry, true);
    this.acceptMask = 1 << (n - 1);
    this.countsBytes = builder.countsBytes;
  }

  matches(str, s0, e0) {
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
          if (setHas(clsRefs[s], c)) next |= closures[nexts[s]];
        } else if (k === S_ANY) {
          next |= closures[nexts[s]];
        }
      }
      active = next;
    }
    return (active & this.acceptMask) !== 0;
  }
}

function anySeg(builder, entry, init, test) {
  const kinds = builder.kinds;
  const n = kinds.length;
  let any = false;
  for (let s = 0; s < n && !any; s++) any = kinds[s] === S_ANY && test(init, s);
  if (!any) return false;
  const scratch = n <= LENS_SCRATCH;
  const lo = scratch ? LENS_LO.fill(0, 0, n) : new Int32Array(n);
  const hi = scratch ? LENS_HI.fill(0, 0, n) : new Int32Array(n);
  const tail = (scratch ? LENS_TAIL : new Float64Array(n)).fill(Infinity, 0, n);
  lo[entry] = 1;
  wildLengths(builder, lo, hi, tail);
  return allNonEmpty(lo[n - 1], hi[n - 1], tail[n - 1]);
}

function wildLengths(nfa, lo, hi, tail) {
  const kinds = nfa.kinds;
  const nexts = nfa.nexts;
  const splitBs = nfa.splitBs;
  for (let s = 0; s < kinds.length; s++) {
    const l = lo[s];
    const h = hi[s];
    let f = tail[s];
    if (l === 0 && h === 0 && f === Infinity) continue;
    const k = kinds[s];
    if (k === S_SPLIT) {
      addLens(lo, hi, tail, nexts[s], l, h, f);
      addLens(lo, hi, tail, splitBs[s], l, h, f);
    } else if (k === S_ANY && nexts[s] === s) {
      const least = l !== 0 ? ctz32(l) : h !== 0 ? 32 + ctz32(h) : f;
      if (least < f) f = tail[s] = least;
      addLens(lo, hi, tail, splitBs[s], l, h, f);
    } else if (k === S_ANY) {
      addLens(lo, hi, tail, nexts[s], l << 1, (h << 1) | (l >>> 31), f + 1);
    }
  }
}

function addLens(lo, hi, tail, t, l, h, f) {
  if (t === UNSET) return;
  lo[t] |= l;
  hi[t] |= h;
  if (f < tail[t]) tail[t] = f;
}

function allNonEmpty(lo, hi, tail) {
  if (tail < 32) {
    lo |= -1 << tail;
    hi = -1;
  } else if (tail < 64) hi |= -1 << (tail - 32);
  return tail <= 64 && (lo | 1) === -1 && hi === -1;
}

class SegNfaWide {
  constructor(builder, entry, dot) {
    this.kinds = Uint8Array.from(builder.kinds);
    this.byteVals = Uint8Array.from(builder.byteVals);
    this.nexts = Int32Array.from(builder.nexts);
    this.splitBs = Int32Array.from(builder.splitBs);
    this.clsRefs = builder.clsRefs.slice();
    const n = builder.kinds.length;
    const words = wordsFor(n);
    this.closures = wideClosures(builder, words);
    const entrySet = (block) => {
      const set = new Int32Array(words);
      bitSet(set, entry);
      closeWide(this, set, block);
      return set;
    };
    this.init = entrySet(false);
    this.initDotBlocked = dot ? this.init : entrySet(true);
    this.accept = n - 1;
    this.countsBytes = builder.countsBytes;
  }

  matches(str, s0, e0) {
    const protectedStart = e0 > s0 && str.charCodeAt(s0) === 0x2e;
    const active = (protectedStart ? this.initDotBlocked : this.init).slice();
    return bitTest(runWide(this, active, str, s0, e0), this.accept);
  }
}

function runWide(nfa, active, str, s0, e0) {
  let next = new Int32Array(active.length);
  for (let i = s0; i < e0; i++) {
    if (bitsIsZero(active)) break;
    const c = str.charCodeAt(i);
    if (nfa.closures === null) stepClosing(nfa, active, next, c);
    else stepKept(nfa, active, next, c);
    const t = active;
    active = next;
    next = t;
  }
  return active;
}

function stepClosing(nfa, active, next, c) {
  next.fill(0);
  const kinds = nfa.kinds;
  for (let w = 0; w < active.length; w++) {
    let bits = active[w];
    while (bits !== 0) {
      const s = w * 32 + ctz32(bits);
      bits &= bits - 1;
      const k = kinds[s];
      if (
        k === S_ANY ||
        (k === S_BYTE && nfa.byteVals[s] === c) ||
        (k === S_CLASS && setHas(nfa.clsRefs[s], c))
      ) {
        bitSet(next, nfa.nexts[s]);
      }
    }
  }
  closeWide(nfa, next, false);
}

function stepKept(nfa, active, next, c) {
  next.fill(0);
  const words = active.length;
  const kinds = nfa.kinds;
  for (let w = 0; w < words; w++) {
    let bits = active[w];
    while (bits !== 0) {
      const s = w * 32 + ctz32(bits);
      bits &= bits - 1;
      const k = kinds[s];
      if (
        k === S_BYTE
          ? nfa.byteVals[s] === c
          : k === S_CLASS
            ? setHas(nfa.clsRefs[s], c)
            : k === S_ANY
      ) {
        const base = nfa.nexts[s] * words;
        for (let q = 0; q < words; q++) next[q] |= nfa.closures[base + q];
      }
    }
  }
}

function wideClosures(builder, words) {
  const n = builder.kinds.length;
  if (n * words * 4 > MAX_CLOSURE_BYTES) return null;
  const closures = new Int32Array(n * words);
  const copy = (s, from) => {
    for (let k = 0; k < words; k++) closures[s * words + k] |= closures[from * words + k];
  };
  for (let s = n - 1; s >= 0; s--) {
    const k = builder.kinds[s];
    if (k === S_SPLIT) {
      copy(s, builder.nexts[s]);
      if (builder.splitBs[s] !== UNSET) copy(s, builder.splitBs[s]);
    } else {
      if (k === S_ANY && builder.nexts[s] === s) copy(s, builder.splitBs[s]);
      closures[s * words + (s >>> 5)] |= 1 << (s & 31);
    }
  }
  return closures;
}

export class GlueNfa {
  constructor(elems, succs, glued, pieces, ci) {
    const n = elems.length;
    const gluedTo = (t) => t !== ACCEPT && glued[t] === 1;
    const builder = new SegBuilder(ci);
    const spanLo = new Int32Array(n);
    const spanHi = new Int32Array(n);
    const waiting = [];
    for (let i = 0; i < n; i++) {
      const succ = succs[i];
      if (glued[i] === 0 && !succ.some(gluedTo)) continue;
      const first = builder.kinds.length;
      const e = elems[i];
      let nodes;
      if (e.kind === EL_LIT) nodes = [lit(e.litStr)];
      else if (e.kind === EL_WILD) nodes = pieces[i];
      else if (e.kind === EL_JOIN) nodes = [];
      else throw new Error("a globstar is never glued");
      const entry = builder.compileNodes(nodes);
      for (const s of waiting[i] ?? []) builder.patch(s, entry);
      const tails = builder.tails;
      builder.tails = [];
      const exit = succ.some((t) => !gluedTo(t));
      const glues = succ.filter(gluedTo);
      const ways = (exit ? 1 : 0) + glues.length;
      const fan = builder.kinds.length;
      for (let k = 1; k < ways; k++) builder.alloc(S_SPLIT, 0, UNSET);
      const leaves = [];
      if (exit) leaves.push(builder.alloc(S_EXIT, 0, i));
      for (const t of glues) {
        const s = builder.alloc(S_SPLIT, 0, UNSET);
        (waiting[t] ??= []).push(s);
        leaves.push(s);
      }
      const fanEntry = builder.fan(fan, leaves);
      for (const t of tails) builder.patch(t, fanEntry);
      spanLo[i] = first;
      spanHi[i] = builder.kinds.length;
      elems[i] = { kind: EL_PIECE, litStr: null, wild: null, entry: glued[i] ? INNER : entry };
    }
    this.kinds = Uint8Array.from(builder.kinds);
    this.byteVals = Uint8Array.from(builder.byteVals);
    this.nexts = Int32Array.from(builder.nexts);
    this.splitBs = Int32Array.from(builder.splitBs);
    this.clsRefs = builder.clsRefs.slice();
    this.words = wordsFor(builder.kinds.length);
    this.closures = wideClosures(builder, this.words);
    this.countsBytes = builder.countsBytes;
    this.spanLo = spanLo;
    this.spanHi = spanHi;
    this.lengths = null;
  }

  set() {
    return new Int32Array(this.words);
  }

  exits(active, str, s0, e0, dot, byteOnly, exit) {
    if (this.countsBytes && !byteOnly && !isAscii(str, s0, e0)) {
      str = utf8Latin1(str.slice(s0, e0));
      s0 = 0;
      e0 = str.length;
    }
    closeWide(this, active, !dot && e0 > s0 && str.charCodeAt(s0) === 0x2e);
    active = runWide(this, active, str, s0, e0);
    for (let w = 0; w < active.length; w++) {
      for (let bits = active[w]; bits !== 0; bits &= bits - 1) {
        const s = w * 32 + ctz32(bits);
        if (this.kinds[s] === S_EXIT) exit(this.nexts[s]);
      }
    }
  }

  anyExits(entries, exit) {
    const n = this.kinds.length;
    this.lengths ??= { lo: new Int32Array(n), hi: new Int32Array(n), tail: new Float64Array(n) };
    const { lo, hi, tail } = this.lengths;
    lo.fill(0);
    hi.fill(0);
    tail.fill(Infinity);
    for (let w = 0; w < entries.length; w++) {
      for (let bits = entries[w]; bits !== 0; bits &= bits - 1) lo[w * 32 + ctz32(bits)] |= 1;
    }
    wildLengths(this, lo, hi, tail);
    for (let s = 0; s < n; s++) {
      if (this.kinds[s] === S_EXIT && allNonEmpty(lo[s], hi[s], tail[s])) exit(this.nexts[s]);
    }
  }

  reach(node, exitOn, good) {
    const lo = this.spanLo[node];
    const hi = this.spanHi[node];
    reach(this, lo, hi, exitOn, good);
    return lo < hi && good[lo] === 1;
  }
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
    this.countsBytes = false;
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
    if (this.kinds[state] === S_ANY && this.nexts[state] === state) this.splitBs[state] = target;
    else this.nexts[state] = target;
  }

  fan(first, leaves) {
    const last = leaves.length - 1;
    for (let k = 0; k < last; k++) {
      this.nexts[first + k] = leaves[k];
      this.splitBs[first + k] = k + 1 < last ? first + k + 1 : leaves[last];
    }
    return last === 0 ? leaves[0] : first;
  }

  compileNodes(nodes) {
    if (nodes.length === 0) {
      const s = this.alloc(S_SPLIT, 0, UNSET);
      this.tails.push(s);
      return s;
    }
    let entry = -1;
    let pending = [];
    for (const node of nodes) {
      const [nodeEntry, nodeTails] = this.compileNode(node);
      for (const t of pending) this.patch(t, nodeEntry);
      pending = nodeTails;
      if (entry === -1) entry = nodeEntry;
    }
    for (const t of pending) this.tails.push(t);
    return entry;
  }

  compileNode(node) {
    switch (node.tag) {
      case N_LITERAL: {
        const str = node.str;
        const entry = this.litState(str.charCodeAt(0));
        let prev = entry;
        for (let i = 1; i < str.length; i++) {
          const s = this.litState(str.charCodeAt(i));
          this.patch(prev, s);
          prev = s;
        }
        return [entry, [prev]];
      }
      case N_ANYCHAR: {
        this.countsBytes = true;
        const s = this.alloc(S_ANY, 0, UNSET);
        return [s, [s]];
      }
      case N_CLASS: {
        if (node.neg) this.countsBytes = true;
        const s = this.alloc(S_CLASS, 0, UNSET, UNSET, byteSet(node, this.ci));
        return [s, [s]];
      }
      case N_STAR: {
        const s = this.alloc(S_ANY, 0, this.kinds.length);
        return [s, [s]];
      }
      case N_BRACE: {
        const first = this.kinds.length;
        for (let i = 1; i < node.branches.length; i++) this.alloc(S_SPLIT, 0, UNSET);
        const entries = [];
        const tails = [];
        for (const branch of node.branches) {
          entries.push(this.compileNodes(seq(branch)));
          for (const t of this.tails) tails.push(t);
          this.tails = [];
        }
        return [this.fan(first, entries), tails];
      }
      default:
        throw new Error("separator-crossing node inside a segment");
    }
  }

  litState(b) {
    const cls = this.ci ? letterClass(b) : null;
    if (cls !== null) return this.alloc(S_CLASS, 0, UNSET, UNSET, cls);
    return this.alloc(S_BYTE, b, UNSET);
  }
}
