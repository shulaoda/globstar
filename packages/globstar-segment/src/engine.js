// SSM — segment-structured matcher. JS port of the Rust crate
// `globstar-segment` (crates/globstar-segment/src/engine/); see
// `references/decisions/segment-engine-design.md`.
//
// One algorithm, two execution modes:
//
// - **String mode** (default): matches directly on the JS string with
//   `charCodeAt` / `startsWith` / `endsWith` / `indexOf` intrinsics —
//   zero per-call allocation, no UTF-8 encode. Byte semantics are
//   preserved exactly: literal compares and `*`/globstar absorption
//   are provably unit-count-independent, and the only two constructs
//   whose semantics depend on *counting* (`?`, which must consume one
//   BYTE, and negated classes, which match one byte) BAIL to byte
//   mode when they would touch a char code > 0x7F. Patterns that
//   contain non-ASCII bytes anywhere compile to byte mode outright.
// - **Byte mode**: `toBytes(input)` once, same algorithm over the
//   `Uint8Array`. Also used for `Uint8Array` inputs.
//
// Compile is a linear scan over the lowered ops — no subset
// construction, no hash maps. Patterns the segment model cannot
// express (globstars glued mid-segment via braces, escaped
// separators inside literals, fork/element/state budget overflows)
// return `null` from `build` and the caller falls back to the PikeVm.

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
  computeStaticPrefixes,
} from "../../globstar/src/matcher/engine/ops.js";
import { CI_BYTE, classMatches, klass, classItemByte } from "../../globstar/src/matcher/ast.js";
import { isPathSep, eqByteCi, asciiCaseAlt } from "../../globstar/src/matcher/options.js";
import { toBytes } from "../../globstar/src/matcher/utf8.js";
import { DirMatch } from "../../globstar/src/matcher/dir-match.js";

// Budgets (element NFA and in-segment NFA live in 32-bit masks here;
// Rust uses 64 — overflow just means the pattern takes the PikeVM
// fallback in JS, with identical results).
const MAX_FORKS = 64;
const MAX_SEQ_STATES = 32;
const MAX_SEG_NFA_STATES = 32;

// Element kinds.
const EL_LIT = 0;
const EL_WILD = 1;
const EL_G0 = 2; // absorb >= 0 segments
const EL_G0_STRICT = 3; // absorb >= 0, first absorbed segment nonempty
const EL_G1 = 4; // absorb >= 1 segment

// Wild kinds.
const WK_AFFIX = 0;
const WK_AFFIX_SET = 1;
const WK_GENERIC = 2;

// Tri-state results for string-mode matchers.
const NO = 0;
const YES = 1;
const BAIL = 2;

const MAX_SUFFIX_PRODUCT = 16;

const ctz32 = (v) => 31 - Math.clz32(v & -v);

// ---------------------------------------------------------------------------
// Compiled shape
// ---------------------------------------------------------------------------

function makeElem(kind, litBytes, wild) {
  // One hidden class for every element keeps the match loops
  // monomorphic.
  return {
    kind,
    litBytes,
    litStr: litBytes !== null ? latin1(litBytes) : null,
    wild,
  };
}

function latin1(bytes) {
  // Pattern literals are short; `apply` accepts the typed array as
  // an arguments list directly.
  return bytes.length === 0 ? "" : String.fromCharCode.apply(null, bytes);
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

export class SegmentMatcher {
  constructor(seqs, program, byteOnly, dot) {
    this.seqs = seqs;
    this.facts = program.facts;
    this.ci = !!program.caseInsensitive;
    this.dot = dot;
    this.byteOnly = byteOnly;
    // Consumes JS strings natively — `makeMatcher` skips `toBytes`.
    this.acceptsStrings = true;
    // Eager on both runtimes: a cheap leading-literal scan, and it
    // lets the matcher drop every reference to the op tree (lazy
    // computation would retain `program.ops` for the matcher's whole
    // lifetime).
    this.prefixes = computeStaticPrefixes(program.ops);
    // On posix without case folding, sep-aware suffix compare is
    // plain equality — `String.prototype.endsWith` applies.
    this.factsPlain = !this.ci && !IS_WINDOWS_SEP;
    // String forms of the facts prefilter so string mode never
    // touches bytes.
    const f = program.facts;
    this.factsSuffixStr = f.suffix.length > 0 ? latin1(f.suffix) : null;
    this.factsSuffixSetStr =
      f.suffixSet.length > 0 ? f.suffixSet.map(latin1) : null;
  }

  /// `null` ⇒ not segment-expressible; caller falls back.
  static build(program, dot) {
    const opSeqs = expandForks(program.ops);
    if (opSeqs === null) return null;
    // Fork expansion introduces no new bytes — one scan of the
    // original ops decides the mode.
    const byteOnly = opsHaveNonAscii(program.ops);
    const seqs = [];
    for (const ops of opSeqs) {
      const seq = segmentize(ops, dot, !!program.caseInsensitive);
      if (seq === null) return null;
      seqs.push(seq);
    }
    return new SegmentMatcher(seqs, program, byteOnly, dot);
  }

  staticPrefixes() {
    return this.prefixes;
  }

  isMatch(input) {
    if (typeof input === "string") {
      if (!this.byteOnly) {
        const r = this._isMatchStr(input);
        if (r !== BAIL) return r === YES;
      }
      return this._isMatchBytes(toBytes(input));
    }
    return this._isMatchBytes(input);
  }

  matchDir(input) {
    if (typeof input === "string") {
      if (!this.byteOnly) {
        const r = this._matchDirStr(input);
        if (r !== -1) return r;
      }
      return this._matchDirBytes(toBytes(input));
    }
    return this._matchDirBytes(input);
  }

  // ---- string mode ----

  _isMatchStr(str) {
    if (!this._factsAcceptStr(str)) return NO;
    const seqs = this.seqs;
    // Fork-local suffix prefilter (multi-fork only; skipped under ci
    // — it is an optimization, the full match re-checks everything).
    const quick = seqs.length > 1 && !this.ci;
    let bailed = false;
    for (let i = 0; i < seqs.length; i++) {
      const seq = seqs[i];
      if (quick && seq.quickSuffixStr.length > 0 && !str.endsWith(seq.quickSuffixStr)) continue;
      const r = seqMatchesStr(seq, str, this.dot, this.ci);
      if (r === YES) return YES;
      if (r === BAIL) bailed = true;
    }
    return bailed ? BAIL : NO;
  }

  _factsAcceptStr(str) {
    const plain = this.factsPlain;
    const suf = this.factsSuffixStr;
    if (suf !== null) {
      return plain ? str.endsWith(suf) : endsWithSepAwareStr(str, suf, this.ci);
    }
    const set = this.factsSuffixSetStr;
    if (set !== null) {
      for (let i = 0; i < set.length; i++) {
        if (plain ? str.endsWith(set[i]) : endsWithSepAwareStr(str, set[i], this.ci)) return true;
      }
      return false;
    }
    return true;
  }

  // -1 ⇒ bail to byte mode.
  _matchDirStr(str) {
    let exact = false;
    let prefix = false;
    const seqs = this.seqs;
    for (let i = 0; i < seqs.length; i++) {
      const active = nfaRunStr(seqs[i], str, this.dot, this.ci);
      if (active === -1) return -1;
      if ((active & acceptBit(seqs[i])) !== 0) exact = true;
      if ((active & seqs[i].reach1) !== 0) prefix = true;
      if (exact && prefix) break;
    }
    return DirMatch.fromExactPrefix(exact, prefix);
  }

  // ---- byte mode ----

  _isMatchBytes(bytes) {
    if (!this.facts.accept(bytes)) return false;
    const seqs = this.seqs;
    const quick = seqs.length > 1 && !this.ci;
    for (let i = 0; i < seqs.length; i++) {
      const seq = seqs[i];
      const qs = seq.quickSuffixBytes;
      if (
        quick &&
        qs.length > 0 &&
        (bytes.length < qs.length || !affixEqBytes(qs, bytes, bytes.length - qs.length, false))
      ) {
        continue;
      }
      if (seqMatchesBytes(seq, bytes, this.dot, this.ci)) return true;
    }
    return false;
  }

  _matchDirBytes(bytes) {
    let exact = false;
    let prefix = false;
    const seqs = this.seqs;
    for (let i = 0; i < seqs.length; i++) {
      const active = nfaRunBytes(seqs[i], bytes, this.dot, this.ci);
      if ((active & acceptBit(seqs[i])) !== 0) exact = true;
      if ((active & seqs[i].reach1) !== 0) prefix = true;
      if (exact && prefix) break;
    }
    return DirMatch.fromExactPrefix(exact, prefix);
  }
}

// ---------------------------------------------------------------------------
// Compilation — fork expansion + segmentizer (ports engine/compile.rs)
// ---------------------------------------------------------------------------

function opCrossesSegment(op) {
  switch (op.kind) {
    case OP_SEP:
    case OP_SEP_RUN:
    case OP_GLOBSTAR:
    case OP_OPT_SEGMENTS_SLASH:
    case OP_SLASH_ANYTHING:
    case OP_GLOBSTAR_ANY:
    case OP_LEADING_SEPS:
      return true;
    case OP_ALTERNATION:
      for (const b of op.branches) {
        for (const o of b) if (opCrossesSegment(o)) return true;
      }
      return false;
    default:
      return false;
  }
}

function opIsCrossingAlt(op) {
  if (op.kind !== OP_ALTERNATION) return false;
  for (const b of op.branches) {
    for (const o of b) if (opCrossesSegment(o)) return true;
  }
  return false;
}

function expandForks(ops) {
  let crossing = false;
  for (const op of ops) {
    if (opIsCrossingAlt(op)) {
      crossing = true;
      break;
    }
  }
  if (!crossing) return [ops];
  let seqs = [[]];
  for (const op of ops) {
    if (opIsCrossingAlt(op)) {
      const expanded = [];
      for (const branch of op.branches) {
        const sub = expandForks(branch);
        if (sub === null) return null;
        for (const s of sub) expanded.push(s);
        if (expanded.length > MAX_FORKS) return null;
      }
      const next = [];
      for (const seq of seqs) {
        for (const exp of expanded) {
          if (next.length >= MAX_FORKS) return null;
          next.push(seq.concat(exp));
        }
      }
      seqs = next;
    } else {
      for (const seq of seqs) seq.push(op);
    }
  }
  return seqs;
}

function opsHaveNonAscii(ops) {
  for (const op of ops) {
    switch (op.kind) {
      case OP_LIT:
        for (let i = 0; i < op.bytes.length; i++) {
          if (op.bytes[i] > 0x7f) return true;
        }
        break;
      case OP_CLASS:
        for (const it of op.cls.items) {
          if (it.tag === CI_BYTE ? it.b > 0x7f : it.hi > 0x7f) return true;
        }
        break;
      case OP_ALTERNATION:
        for (const b of op.branches) {
          if (opsHaveNonAscii(b)) return true;
        }
        break;
      default:
        break;
    }
  }
  return false;
}

function litContainsSep(op) {
  if (op.kind === OP_LIT) {
    for (let i = 0; i < op.bytes.length; i++) {
      if (isPathSep(op.bytes[i])) return true;
    }
    return false;
  }
  if (op.kind === OP_ALTERNATION) {
    for (const b of op.branches) {
      for (const o of b) if (litContainsSep(o)) return true;
    }
  }
  return false;
}

// Boundary states while segmentizing.
const B_FRESH = 0;
const B_STRICT = 1;
const B_LENIENT = 2;
const B_IN_SEGMENT = 3;

const EMPTY_BYTES = new Uint8Array(0);

function segmentize(ops, dot, ci) {
  const elems = [];
  let buf = [];
  let state = B_FRESH;
  let gOpen = false;
  let gUpgradeable = false;
  let leadingSeps = false;

  const closeSegment = () => {
    if (buf.length === 0) return makeElem(EL_LIT, EMPTY_BYTES, null);
    const segOps = buf;
    buf = [];
    if (segOps.length === 1 && segOps[0].kind === OP_LIT) {
      return makeElem(EL_LIT, segOps[0].bytes, null);
    }
    const wild = compileWild(segOps, dot, ci);
    return wild === null ? null : makeElem(EL_WILD, null, wild);
  };

  for (let i = 0; i < ops.length; i++) {
    const op = ops[i];
    switch (op.kind) {
      case OP_LIT:
      case OP_ANYCHAR:
      case OP_STAR:
      case OP_CLASS:
      case OP_ALTERNATION: {
        if (gOpen) return null; // `.*` glued to segment content
        if (litContainsSep(op)) return null; // escaped separator
        pushInSeg(buf, op);
        state = B_IN_SEGMENT;
        break;
      }
      case OP_SEP: {
        if (gOpen) {
          // The separator is the open absorber's right boundary; a
          // `.*` before a mandatory `/` must absorb >= 1 segment.
          if (gUpgradeable) elems[elems.length - 1] = makeElem(EL_G1, null, null);
          gOpen = false;
          gUpgradeable = false;
          state = B_FRESH;
        } else {
          const e = closeSegment();
          if (e === null) return null;
          elems.push(e);
          state = B_STRICT;
        }
        break;
      }
      case OP_SEP_RUN: {
        if (gOpen) return null;
        const e = closeSegment();
        if (e === null) return null;
        elems.push(e);
        state = B_LENIENT;
        break;
      }
      case OP_LEADING_SEPS: {
        if (i !== 0) return null;
        leadingSeps = true;
        break;
      }
      case OP_OPT_SEGMENTS_SLASH: {
        if (buf.length > 0 || state === B_IN_SEGMENT || gOpen) return null;
        let strictEntry;
        if (state === B_FRESH) strictEntry = !leadingSeps && elems.length > 0;
        else if (state === B_STRICT) strictEntry = true;
        else strictEntry = false; // B_LENIENT
        elems.push(makeElem(strictEntry ? EL_G0_STRICT : EL_G0, null, null));
        state = B_FRESH;
        leadingSeps = false;
        break;
      }
      case OP_SLASH_ANYTHING: {
        if (gOpen) return null;
        const e = closeSegment();
        if (e === null) return null;
        elems.push(e);
        elems.push(makeElem(EL_G1, null, null));
        gOpen = true;
        gUpgradeable = false;
        state = B_FRESH;
        break;
      }
      case OP_GLOBSTAR_ANY: {
        if (buf.length > 0 || state === B_IN_SEGMENT || gOpen) return null;
        const strict = state === B_STRICT;
        elems.push(makeElem(strict ? EL_G1 : EL_G0, null, null));
        gOpen = true;
        gUpgradeable = !strict;
        state = B_FRESH;
        break;
      }
      default:
        return null; // raw OP_GLOBSTAR never survives the fold
    }
  }

  if (!gOpen) {
    const e = closeSegment();
    if (e === null) return null;
    elems.push(e);
  }
  return finishSeq(elems);
}

function pushInSeg(buf, op) {
  if (op.kind === OP_LIT && buf.length > 0 && buf[buf.length - 1].kind === OP_LIT) {
    const prev = buf[buf.length - 1];
    const merged = new Uint8Array(prev.bytes.length + op.bytes.length);
    merged.set(prev.bytes, 0);
    merged.set(op.bytes, prev.bytes.length);
    buf[buf.length - 1] = { kind: OP_LIT, bytes: merged };
    return;
  }
  buf.push(op);
}

// ---------------------------------------------------------------------------
// Wild classification
// ---------------------------------------------------------------------------

function makeWild(kind, fields) {
  return {
    kind,
    prefixBytes: fields.prefixBytes ?? EMPTY_BYTES,
    prefixStr: fields.prefixStr ?? "",
    suffixBytes: fields.suffixBytes ?? EMPTY_BYTES,
    suffixStr: fields.suffixStr ?? "",
    suffixSetBytes: fields.suffixSetBytes ?? null,
    suffixSetStr: fields.suffixSetStr ?? null,
    minLen: fields.minLen ?? 0,
    variable: fields.variable ?? true,
    dotProtect: fields.dotProtect ?? false,
    anychars: fields.anychars ?? 0,
    nfa: fields.nfa ?? null,
  };
}

function compileWild(ops, dot, ci) {
  let idx = 0;
  let prefix = EMPTY_BYTES;
  if (ops[0].kind === OP_LIT) {
    prefix = ops[0].bytes;
    idx = 1;
  }
  let anychars = 0;
  let hasStar = false;
  while (idx < ops.length) {
    const k = ops[idx].kind;
    if (k === OP_STAR) hasStar = true;
    else if (k === OP_ANYCHAR) anychars++;
    else break;
    idx++;
  }
  const hasWilds = hasStar || anychars > 0;
  const dotProtect = !dot && prefix.length === 0 && hasWilds;

  if (idx === ops.length) {
    return makeWild(WK_AFFIX, {
      prefixBytes: prefix,
      prefixStr: latin1(prefix),
      minLen: prefix.length + anychars,
      variable: hasStar,
      dotProtect,
      anychars,
    });
  }
  const suffixes = suffixProduct(ops, idx);
  if (suffixes !== null) {
    if (suffixes.length === 1) {
      return makeWild(WK_AFFIX, {
        prefixBytes: prefix,
        prefixStr: latin1(prefix),
        suffixBytes: suffixes[0],
        suffixStr: latin1(suffixes[0]),
        minLen: prefix.length + suffixes[0].length + anychars,
        variable: hasStar,
        dotProtect,
        anychars,
      });
    }
    return makeWild(WK_AFFIX_SET, {
      prefixBytes: prefix,
      prefixStr: latin1(prefix),
      suffixSetBytes: suffixes,
      suffixSetStr: suffixes.map(latin1),
      minLen: prefix.length + anychars,
      variable: hasStar,
      dotProtect,
      anychars,
    });
  }
  const nfa = SegNfa.compile(ops, dot, ci);
  if (nfa === null) return null;
  return makeWild(WK_GENERIC, {
    dotProtect: !dot && nfa.wildLed,
    nfa,
  });
}

function suffixProduct(ops, from) {
  // Overwhelmingly common tail: one literal op (`*.ts`).
  if (from + 1 === ops.length && ops[from].kind === OP_LIT) {
    return [ops[from].bytes];
  }
  let parts = [[]];
  for (let i = from; i < ops.length; i++) {
    const op = ops[i];
    if (op.kind === OP_LIT) {
      for (const p of parts) {
        for (let j = 0; j < op.bytes.length; j++) p.push(op.bytes[j]);
      }
    } else if (op.kind === OP_ALTERNATION) {
      const lits = [];
      for (const b of op.branches) {
        if (b.length === 0) lits.push(EMPTY_BYTES);
        else if (b.length === 1 && b[0].kind === OP_LIT) lits.push(b[0].bytes);
        else return null;
      }
      if (parts.length * lits.length > MAX_SUFFIX_PRODUCT) return null;
      const next = [];
      for (const p of parts) {
        for (const l of lits) {
          const v = p.slice();
          for (let j = 0; j < l.length; j++) v.push(l[j]);
          next.push(v);
        }
      }
      parts = next;
    } else {
      return null;
    }
  }
  return parts.map((p) => Uint8Array.from(p));
}

// ---------------------------------------------------------------------------
// Element-NFA metadata (ports `finish` in engine/compile.rs)
// ---------------------------------------------------------------------------

function finishSeq(elems) {
  const m = elems.length;
  const stateOf = [];
  let n = 0;
  for (const e of elems) {
    stateOf.push(n);
    n += e.kind === EL_G0_STRICT || e.kind === EL_G1 ? 2 : 1;
    if (n >= MAX_SEQ_STATES) return null;
  }
  const accept = n;
  n += 1;

  // Inverse map: owning element per state (accept slot unused).
  // Plain array — a tiny typed array costs more in wrapper overhead
  // than it saves in slot width.
  const elemOf = new Array(n).fill(0);
  for (let i = 0; i < m; i++) {
    const end = i + 1 < m ? stateOf[i + 1] : accept;
    for (let s = stateOf[i]; s < end; s++) elemOf[s] = i;
  }

  // Plain exact-length array of SMI masks — cheaper to retain than a
  // small typed array (no separate backing-buffer object).
  const eps = new Array(n);
  for (let s = 0; s < n; s++) eps[s] = 1 << s;
  for (let i = m - 1; i >= 0; i--) {
    const s = stateOf[i];
    const nextEntry = i + 1 < m ? stateOf[i + 1] : accept;
    const k = elems[i].kind;
    if (k === EL_G0) eps[s] |= eps[nextEntry];
    else if (k === EL_G0_STRICT) {
      eps[s] |= eps[nextEntry];
      eps[s + 1] |= eps[nextEntry];
    } else if (k === EL_G1) {
      eps[s + 1] |= eps[nextEntry];
    }
  }

  const satFrom = new Array(m + 1).fill(true);
  for (let i = m - 1; i >= 0; i--) {
    satFrom[i] = elemSatisfiable(elems[i]) && satFrom[i + 1];
  }
  let reach1 = 0;
  for (let i = 0; i < m; i++) {
    const s = stateOf[i];
    const k = elems[i].kind;
    const isG = k === EL_G0 || k === EL_G0_STRICT || k === EL_G1;
    const can = isG ? satFrom[i + 1] : satFrom[i];
    if (can) {
      reach1 |= 1 << s;
      if (k === EL_G0_STRICT || k === EL_G1) reach1 |= 1 << (s + 1);
    }
  }

  let gCount = 0;
  let singleG = -1;
  for (let i = 0; i < m; i++) {
    const k = elems[i].kind;
    if (k === EL_G0 || k === EL_G0_STRICT || k === EL_G1) {
      gCount++;
      if (singleG === -1) singleG = i;
    }
  }
  if (gCount !== 1) singleG = -1;

  // Pre-join all-literal heads for the single-globstar fast path
  // (mirrors Rust `finish`): "src/**/…" heads become one prefix
  // compare instead of a segment iteration. String form only — byte
  // mode is the rare path and keeps the per-segment loop.
  let joinedHeadStr = null;
  if (gCount === 1 && singleG > 0) {
    let allLit = true;
    for (let i = 0; i < singleG; i++) {
      if (elems[i].kind !== EL_LIT) {
        allLit = false;
        break;
      }
    }
    if (allLit) {
      let h = "";
      for (let i = 0; i < singleG; i++) h += elems[i].litStr + "/";
      joinedHeadStr = h;
    }
  }

  // Per-fork quick-reject suffix from the final element (only
  // consulted by multi-fork matchers).
  let quickBytes = EMPTY_BYTES;
  const lastEl = elems[m - 1];
  if (lastEl.kind === EL_LIT) quickBytes = lastEl.litBytes;
  else if (lastEl.kind === EL_WILD && lastEl.wild.kind === WK_AFFIX) {
    quickBytes = lastEl.wild.suffixBytes;
  }

  return {
    elems,
    singleG,
    gCount,
    stateOf,
    elemOf,
    numStates: n,
    eps,
    reach1,
    quickSuffixBytes: quickBytes,
    quickSuffixStr: latin1(quickBytes),
    joinedHeadStr,
  };
}

function elemSatisfiable(e) {
  if (e.kind !== EL_WILD) return true;
  const w = e.wild;
  return w.kind === WK_GENERIC ? w.nfa.satisfiable : true;
}

function acceptBit(seq) {
  return 1 << (seq.numStates - 1);
}

// ---------------------------------------------------------------------------
// Matching — string mode
// ---------------------------------------------------------------------------

function seqMatchesStr(seq, str, dot, ci) {
  if (seq.gCount === 0) return matchFixedStr(seq, str, ci);
  if (seq.gCount === 1) return matchSingleGStr(seq, str, dot, ci);
  const active = nfaRunStr(seq, str, dot, ci);
  if (active === -1) return BAIL;
  return (active & acceptBit(seq)) !== 0 ? YES : NO;
}

function nextSepStr(str, from) {
  // `/` is the overwhelmingly common separator; use the intrinsic.
  const i = str.indexOf("/", from);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.indexOf("\\", from);
  if (i === -1) return j;
  if (j === -1) return i;
  return i < j ? i : j;
}

const IS_WINDOWS_SEP = isPathSep(0x5c);

function matchFixedStr(seq, str, ci) {
  const elems = seq.elems;
  const m = elems.length;
  let pos = 0;
  for (let i = 0; i < m; i++) {
    let end = nextSepStr(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    if (i + 1 < m) {
      if (last) return NO; // fewer segments than elements
    } else if (!last) {
      return NO; // more segments than elements
    }
    const r = elemConsumesStr(elems[i], str, pos, end, ci);
    if (r !== YES) return r;
    pos = end + 1;
  }
  return YES;
}

function matchSingleGStr(seq, str, dot, ci) {
  const elems = seq.elems;
  const g = seq.singleG;
  const m = elems.length;
  const tailLen = m - g - 1;

  // Tail, right-to-left.
  let tailEnd = str.length;
  let ts = 0;
  for (let j = tailLen - 1; j >= 0; j--) {
    let s = lastSepBeforeStr(str, tailEnd);
    s = s === -1 ? 0 : s + 1;
    const r = elemConsumesStr(elems[g + 1 + j], str, s, tailEnd, ci);
    if (r !== YES) return r;
    if (j > 0) {
      if (s === 0) return NO;
      tailEnd = s - 1;
    }
    ts = s;
  }

  // Head: all-literal heads ("src/**/…") compare as one pre-joined
  // sep-aware prefix (mirrors Rust `match_single_g`).
  let midStart;
  let headExhausted = false;
  const jh = seq.joinedHeadStr;
  if (jh !== null) {
    // The joined head includes the separator after each head
    // segment; a shorter path can never match.
    if (str.length < jh.length) return NO;
    if (!ci && !IS_WINDOWS_SEP) {
      if (!str.startsWith(jh)) return NO;
    } else {
      for (let i = 0; i < jh.length; i++) {
        const hb = jh.charCodeAt(i);
        const pb = str.charCodeAt(i);
        if (hb === 0x2f ? !isPathSep(pb) : ci ? !eqByteCi(hb, pb) : hb !== pb) return NO;
      }
    }
    midStart = jh.length;
  } else {
    let pos = 0;
    for (let i = 0; i < g; i++) {
      if (headExhausted) return NO;
      let end = nextSepStr(str, pos);
      if (end === -1) {
        end = str.length;
        headExhausted = true;
      }
      const r = elemConsumesStr(elems[i], str, pos, end, ci);
      if (r !== YES) return r;
      pos = end + 1;
    }
    midStart = pos;
  }

  let midExists;
  let midEnd;
  if (tailLen > 0) {
    if (ts < midStart) return NO; // head/tail overlap
    midExists = ts > midStart;
    midEnd = ts > 0 ? ts - 1 : 0;
  } else {
    midExists = !headExhausted;
    midEnd = str.length;
  }

  const gk = elems[g].kind;
  if (gk === EL_G1) {
    if (!midExists) return NO;
  } else if (gk === EL_G0_STRICT) {
    if (midExists && (midStart >= str.length || isPathSep(str.charCodeAt(midStart)))) {
      return NO;
    }
  }

  if (dot || !midExists) return YES;
  return midStart <= midEnd && hasDotLedSegmentStr(str, midStart, midEnd) ? NO : YES;
}

function lastSepBeforeStr(str, end) {
  const i = str.lastIndexOf("/", end - 1);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.lastIndexOf("\\", end - 1);
  return i > j ? i : j;
}

function hasDotLedSegmentStr(str, start, end) {
  if (start < end && str.charCodeAt(start) === 0x2e) return true;
  let i = start;
  for (;;) {
    i = nextSepStr(str, i);
    if (i === -1 || i + 1 >= end) return false;
    if (str.charCodeAt(i + 1) === 0x2e) return true;
    i += 1;
  }
}

function nfaRunStr(seq, str, dot, ci) {
  let active = seq.eps[seq.stateOf[0]];
  let pos = 0;
  for (;;) {
    if (active === 0) return 0;
    let end = nextSepStr(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    active = nfaStepStr(seq, active, str, pos, end, dot, ci);
    if (active === -1) return -1;
    if (last) return active;
    pos = end + 1;
  }
}

function nfaStepStr(seq, active, str, s0, e0, dot, ci) {
  let next = 0;
  const elems = seq.elems;
  const m = elems.length;
  const stateOf = seq.stateOf;
  const eps = seq.eps;
  const segEmpty = e0 === s0;
  const segDotLed = !segEmpty && str.charCodeAt(s0) === 0x2e;
  const absorbOk = dot || !segDotLed;
  let bits = active;
  while (bits !== 0) {
    const s = ctz32(bits);
    bits &= bits - 1;
    if (s === seq.numStates - 1) continue; // accept
    const i = seq.elemOf[s];
    const entry = stateOf[i];
    const nextEntry = i + 1 < m ? stateOf[i + 1] : seq.numStates - 1;
    const e = elems[i];
    switch (e.kind) {
      case EL_LIT: {
        const r = litEqStr(e.litStr, str, s0, e0, ci);
        if (r === YES) next |= eps[nextEntry];
        break;
      }
      case EL_WILD: {
        const r = wildConsumesStr(e.wild, str, s0, e0, ci);
        if (r === BAIL) return -1;
        if (r === YES) next |= eps[nextEntry];
        break;
      }
      case EL_G0: {
        if (absorbOk) next |= eps[entry];
        break;
      }
      case EL_G0_STRICT: {
        if (absorbOk && !(s === entry && segEmpty)) next |= eps[entry + 1];
        break;
      }
      case EL_G1: {
        if (absorbOk) next |= eps[entry + 1];
        break;
      }
    }
  }
  return next;
}

function elemConsumesStr(e, str, s, t, ci) {
  if (e.kind === EL_LIT) return litEqStr(e.litStr, str, s, t, ci);
  if (e.kind === EL_WILD) return wildConsumesStr(e.wild, str, s, t, ci);
  return NO;
}

function litEqStr(lit, str, s, t, ci) {
  if (t - s !== lit.length) return NO;
  if (!ci) return str.startsWith(lit, s) ? YES : NO;
  for (let i = 0; i < lit.length; i++) {
    if (!eqByteCi(lit.charCodeAt(i), str.charCodeAt(s + i))) return NO;
  }
  return YES;
}

function affixEqStr(part, str, at, ci) {
  if (!ci) return str.startsWith(part, at) ? YES : NO;
  for (let i = 0; i < part.length; i++) {
    if (!eqByteCi(part.charCodeAt(i), str.charCodeAt(at + i))) return NO;
  }
  return YES;
}

function segHasNonAsciiStr(str, s, t) {
  for (let i = s; i < t; i++) {
    if (str.charCodeAt(i) > 0x7f) return true;
  }
  return false;
}

function wildConsumesStr(w, str, s, t, ci) {
  if (w.dotProtect && t > s && str.charCodeAt(s) === 0x2e) return NO;
  const len = t - s;
  switch (w.kind) {
    case WK_AFFIX: {
      // `?` counts BYTES; bail when the segment holds non-ASCII.
      if (w.anychars > 0 && segHasNonAsciiStr(str, s, t)) return BAIL;
      const need = w.minLen;
      if (len < need || (!w.variable && len !== need)) return NO;
      if (w.prefixStr.length > 0 && affixEqStr(w.prefixStr, str, s, ci) === NO) return NO;
      if (
        w.suffixStr.length > 0 &&
        affixEqStr(w.suffixStr, str, t - w.suffixStr.length, ci) === NO
      ) {
        return NO;
      }
      return YES;
    }
    case WK_AFFIX_SET: {
      if (w.anychars > 0 && segHasNonAsciiStr(str, s, t)) return BAIL;
      const p = w.prefixStr;
      if (len < p.length || (p.length > 0 && affixEqStr(p, str, s, ci) === NO)) return NO;
      const set = w.suffixSetStr;
      for (let i = 0; i < set.length; i++) {
        const suf = set[i];
        const need = w.minLen + suf.length;
        if (len < need || (!w.variable && len !== need)) continue;
        if (suf.length === 0 || affixEqStr(suf, str, t - suf.length, ci) !== NO) return YES;
      }
      return NO;
    }
    default: {
      const nfa = w.nfa;
      if (nfa.needsAsciiSeg && segHasNonAsciiStr(str, s, t)) return BAIL;
      return nfa.matchesStr(str, s, t) ? YES : NO;
    }
  }
}

// Separator-aware `endsWith` for the facts prefilter (string form).
function endsWithSepAwareStr(str, suffix, ci) {
  let si = suffix.length;
  let pi = str.length;
  while (si > 0) {
    if (pi === 0) return false;
    si--;
    pi--;
    const sb = suffix.charCodeAt(si);
    const pb = str.charCodeAt(pi);
    if (sb === 0x2f) {
      if (!isPathSep(pb)) return false;
    } else if (ci ? !eqByteCi(sb, pb) : sb !== pb) {
      return false;
    }
  }
  return true;
}

// ---------------------------------------------------------------------------
// Matching — byte mode
// ---------------------------------------------------------------------------

function seqMatchesBytes(seq, bytes, dot, ci) {
  if (seq.gCount === 0) return matchFixedBytes(seq, bytes, ci);
  if (seq.gCount === 1) return matchSingleGBytes(seq, bytes, dot, ci);
  return (nfaRunBytes(seq, bytes, dot, ci) & acceptBit(seq)) !== 0;
}

function nextSepBytes(bytes, from) {
  for (let i = from; i < bytes.length; i++) {
    if (isPathSep(bytes[i])) return i;
  }
  return -1;
}

function matchFixedBytes(seq, bytes, ci) {
  const elems = seq.elems;
  const m = elems.length;
  let pos = 0;
  for (let i = 0; i < m; i++) {
    let end = nextSepBytes(bytes, pos);
    const last = end === -1;
    if (last) end = bytes.length;
    if (i + 1 < m) {
      if (last) return false;
    } else if (!last) {
      return false;
    }
    if (!elemConsumesBytes(elems[i], bytes, pos, end, ci)) return false;
    pos = end + 1;
  }
  return true;
}

function matchSingleGBytes(seq, bytes, dot, ci) {
  const elems = seq.elems;
  const g = seq.singleG;
  const m = elems.length;
  const tailLen = m - g - 1;

  let tailEnd = bytes.length;
  let ts = 0;
  for (let j = tailLen - 1; j >= 0; j--) {
    let s = tailEnd;
    while (s > 0 && !isPathSep(bytes[s - 1])) s--;
    if (!elemConsumesBytes(elems[g + 1 + j], bytes, s, tailEnd, ci)) return false;
    if (j > 0) {
      if (s === 0) return false;
      tailEnd = s - 1;
    }
    ts = s;
  }

  let pos = 0;
  let headExhausted = false;
  for (let i = 0; i < g; i++) {
    if (headExhausted) return false;
    let end = nextSepBytes(bytes, pos);
    if (end === -1) {
      end = bytes.length;
      headExhausted = true;
    }
    if (!elemConsumesBytes(elems[i], bytes, pos, end, ci)) return false;
    pos = end + 1;
  }
  const midStart = pos;

  let midExists;
  let midEnd;
  if (tailLen > 0) {
    if (ts < midStart) return false;
    midExists = ts > midStart;
    midEnd = ts > 0 ? ts - 1 : 0;
  } else {
    midExists = !headExhausted;
    midEnd = bytes.length;
  }

  const gk = elems[g].kind;
  if (gk === EL_G1) {
    if (!midExists) return false;
  } else if (gk === EL_G0_STRICT) {
    if (midExists && (midStart >= bytes.length || isPathSep(bytes[midStart]))) {
      return false;
    }
  }

  if (dot || !midExists) return true;
  return !(midStart <= midEnd && hasDotLedSegmentBytes(bytes, midStart, midEnd));
}

function hasDotLedSegmentBytes(bytes, start, end) {
  if (start < end && bytes[start] === 0x2e) return true;
  for (let i = start; i < end; i++) {
    if (isPathSep(bytes[i]) && i + 1 < end && bytes[i + 1] === 0x2e) return true;
  }
  return false;
}

function nfaRunBytes(seq, bytes, dot, ci) {
  let active = seq.eps[seq.stateOf[0]];
  let pos = 0;
  for (;;) {
    if (active === 0) return 0;
    let end = nextSepBytes(bytes, pos);
    const last = end === -1;
    if (last) end = bytes.length;
    active = nfaStepBytes(seq, active, bytes, pos, end, dot, ci);
    if (last) return active;
    pos = end + 1;
  }
}

function nfaStepBytes(seq, active, bytes, s0, e0, dot, ci) {
  let next = 0;
  const elems = seq.elems;
  const m = elems.length;
  const stateOf = seq.stateOf;
  const eps = seq.eps;
  const segEmpty = e0 === s0;
  const segDotLed = !segEmpty && bytes[s0] === 0x2e;
  const absorbOk = dot || !segDotLed;
  let bits = active;
  while (bits !== 0) {
    const s = ctz32(bits);
    bits &= bits - 1;
    if (s === seq.numStates - 1) continue;
    const i = seq.elemOf[s];
    const entry = stateOf[i];
    const nextEntry = i + 1 < m ? stateOf[i + 1] : seq.numStates - 1;
    const e = elems[i];
    switch (e.kind) {
      case EL_LIT: {
        if (litEqBytes(e.litBytes, bytes, s0, e0, ci)) next |= eps[nextEntry];
        break;
      }
      case EL_WILD: {
        if (wildConsumesBytes(e.wild, bytes, s0, e0, ci)) next |= eps[nextEntry];
        break;
      }
      case EL_G0: {
        if (absorbOk) next |= eps[entry];
        break;
      }
      case EL_G0_STRICT: {
        if (absorbOk && !(s === entry && segEmpty)) next |= eps[entry + 1];
        break;
      }
      case EL_G1: {
        if (absorbOk) next |= eps[entry + 1];
        break;
      }
    }
  }
  return next;
}

function elemConsumesBytes(e, bytes, s, t, ci) {
  if (e.kind === EL_LIT) return litEqBytes(e.litBytes, bytes, s, t, ci);
  if (e.kind === EL_WILD) return wildConsumesBytes(e.wild, bytes, s, t, ci);
  return false;
}

function litEqBytes(lit, bytes, s, t, ci) {
  if (t - s !== lit.length) return false;
  for (let i = 0; i < lit.length; i++) {
    const a = lit[i];
    const b = bytes[s + i];
    if (ci ? !eqByteCi(a, b) : a !== b) return false;
  }
  return true;
}

function affixEqBytes(part, bytes, at, ci) {
  for (let i = 0; i < part.length; i++) {
    const a = part[i];
    const b = bytes[at + i];
    if (ci ? !eqByteCi(a, b) : a !== b) return false;
  }
  return true;
}

function wildConsumesBytes(w, bytes, s, t, ci) {
  if (w.dotProtect && t > s && bytes[s] === 0x2e) return false;
  const len = t - s;
  switch (w.kind) {
    case WK_AFFIX: {
      const need = w.minLen;
      if (len < need || (!w.variable && len !== need)) return false;
      if (w.prefixBytes.length > 0 && !affixEqBytes(w.prefixBytes, bytes, s, ci)) return false;
      if (
        w.suffixBytes.length > 0 &&
        !affixEqBytes(w.suffixBytes, bytes, t - w.suffixBytes.length, ci)
      ) {
        return false;
      }
      return true;
    }
    case WK_AFFIX_SET: {
      const p = w.prefixBytes;
      if (len < p.length || (p.length > 0 && !affixEqBytes(p, bytes, s, ci))) return false;
      const set = w.suffixSetBytes;
      for (let i = 0; i < set.length; i++) {
        const suf = set[i];
        const need = w.minLen + suf.length;
        if (len < need || (!w.variable && len !== need)) continue;
        if (suf.length === 0 || affixEqBytes(suf, bytes, t - suf.length, ci)) return true;
      }
      return false;
    }
    default:
      return w.nfa.matchesBytes(bytes, s, t);
  }
}

// ---------------------------------------------------------------------------
// In-segment mini NFA (Generic wilds) — ports engine/seg_nfa.rs
// ---------------------------------------------------------------------------

const S_BYTE = 0;
const S_CLASS = 1;
const S_ANY = 2;
const S_SPLIT = 3;
const S_JUMP = 4;
const S_DOT_GUARD = 5;
const S_MATCH = 6;

const UNSET = 0xff;

class SegNfa {
  constructor(kinds, byteVals, nexts, dps, splitBs, clsRefs, entry, dot) {
    this.kinds = kinds;
    this.byteVals = byteVals;
    this.nexts = nexts;
    this.dps = dps;
    this.clsRefs = clsRefs;
    this.dot = dot;
    const n = kinds.length;

    // Memoized guard-passing closures: the ε-graph (Split/Jump/Guard
    // edges) is acyclic — every pattern loop passes through a
    // consumer — so each state's closure folds from its children
    // exactly once.
    const closures = new Array(n).fill(-1);
    for (let s = 0; s < n; s++) memoClosure(kinds, nexts, splitBs, closures, s);
    this.closures = closures;
    this.init = closures[entry];
    this.initDotBlocked = closureOfDotBlocked(kinds, nexts, splitBs, entry);
    this.acceptMask = 1 << (n - 1);

    // wildLed: no entry-closure state can consume `.` as a literal or
    // positive class.
    let canLitDot = false;
    let bits = this.init;
    while (bits !== 0) {
      const s = ctz32(bits);
      bits &= bits - 1;
      if (kinds[s] === S_BYTE && byteVals[s] === 0x2e) canLitDot = true;
      else if (kinds[s] === S_CLASS && dps[s] === 0 && classMatches(clsRefs[s], 0x2e)) {
        canLitDot = true;
      }
    }
    this.wildLed = !canLitDot;

    // Bail requirement: `?` (S_ANY from AnyChar OR star bodies feeding
    // non-loop continuations) and negated classes count units. Star
    // bodies are S_ANY too — conservative: any S_ANY or negated class
    // present triggers the segment ASCII scan in string mode.
    let needs = false;
    for (let s = 0; s < n; s++) {
      if (kinds[s] === S_ANY) needs = true;
      if (kinds[s] === S_CLASS && clsRefs[s].neg) needs = true;
    }
    this.needsAsciiSeg = needs;

    this.satisfiable = computeSatisfiable(this);
  }

  static compile(ops, dot, ci) {
    const b = new SegBuilder(ci);
    const entry = b.compileOps(ops, dot);
    if (entry === -1) return null;
    const accept = b.alloc(S_MATCH, 0, UNSET, 0);
    if (accept === -1) return null;
    for (const t of b.tails) b.patch(t, accept);
    // Builder arrays stay plain packed-SMI: at ≤ 32 states an exact
    // typed copy costs compile time and its wrapper overhead cancels
    // the narrower slots.
    return new SegNfa(b.kinds, b.byteVals, b.nexts, b.dps, b.splitBs, b.clsRefs, entry, dot);
  }

  matchesStr(str, s0, e0) {
    const protectedStart = !this.dot && e0 > s0 && str.charCodeAt(s0) === 0x2e;
    let active = protectedStart ? this.initDotBlocked : this.init;
    const kinds = this.kinds;
    const byteVals = this.byteVals;
    const nexts = this.nexts;
    const dps = this.dps;
    const clsRefs = this.clsRefs;
    const closures = this.closures;
    for (let i = s0; i < e0; i++) {
      if (active === 0) return false;
      const c = str.charCodeAt(i);
      const guardDot = i === s0 && !this.dot && c === 0x2e;
      let next = 0;
      let bits = active;
      while (bits !== 0) {
        const s = ctz32(bits);
        bits &= bits - 1;
        const k = kinds[s];
        if (k === S_BYTE) {
          if (byteVals[s] === c) next |= closures[nexts[s]];
        } else if (k === S_CLASS) {
          if (classMatches(clsRefs[s], c) && !(dps[s] !== 0 && guardDot)) {
            next |= closures[nexts[s]];
          }
        } else if (k === S_ANY) {
          if (!(dps[s] !== 0 && guardDot)) next |= closures[nexts[s]];
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
    const dps = this.dps;
    const clsRefs = this.clsRefs;
    const closures = this.closures;
    for (let i = s0; i < e0; i++) {
      if (active === 0) return false;
      const c = bytes[i];
      const guardDot = i === s0 && !this.dot && c === 0x2e;
      let next = 0;
      let bits = active;
      while (bits !== 0) {
        const s = ctz32(bits);
        bits &= bits - 1;
        const k = kinds[s];
        if (k === S_BYTE) {
          if (byteVals[s] === c) next |= closures[nexts[s]];
        } else if (k === S_CLASS) {
          if (classMatches(clsRefs[s], c) && !(dps[s] !== 0 && guardDot)) {
            next |= closures[nexts[s]];
          }
        } else if (k === S_ANY) {
          if (!(dps[s] !== 0 && guardDot)) next |= closures[nexts[s]];
        }
      }
      active = next;
    }
    return (active & this.acceptMask) !== 0;
  }
}

// Memoized guard-passing closure. `memo[s] === -1` means uncomputed;
// the ε-graph is acyclic so plain recursion terminates.
function memoClosure(kinds, nexts, splitBs, memo, s) {
  const cached = memo[s];
  if (cached !== -1) return cached;
  const k = kinds[s];
  let out;
  if (k === S_SPLIT) {
    out =
      memoClosure(kinds, nexts, splitBs, memo, nexts[s]) |
      memoClosure(kinds, nexts, splitBs, memo, splitBs[s]);
  } else if (k === S_JUMP || k === S_DOT_GUARD) {
    out = memoClosure(kinds, nexts, splitBs, memo, nexts[s]);
  } else {
    out = 1 << s;
  }
  memo[s] = out;
  return out;
}

// ε-closure with DotGuard edges BLOCKED — the entry closure for a
// protected leading `.` (guard-passing closures come from memoClosure).
function closureOfDotBlocked(kinds, nexts, splitBs, start) {
  let seen = 0;
  let out = 0;
  const stack = [start];
  while (stack.length > 0) {
    const cur = stack.pop();
    if ((seen & (1 << cur)) !== 0) continue;
    seen |= 1 << cur;
    const k = kinds[cur];
    if (k === S_SPLIT) {
      stack.push(nexts[cur], splitBs[cur]);
    } else if (k === S_JUMP) {
      stack.push(nexts[cur]);
    } else if (k === S_DOT_GUARD) {
      // blocked
    } else {
      out |= 1 << cur;
    }
  }
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
  for (;;) {
    let grew = false;
    let bits = reach & fires;
    while (bits !== 0) {
      const s = ctz32(bits);
      bits &= bits - 1;
      const clo = nfa.closures[nfa.nexts[s]];
      if ((reach | clo) !== reach) {
        reach |= clo;
        grew = true;
      }
    }
    if (!grew) return (reach & nfa.acceptMask) !== 0;
  }
}

class SegBuilder {
  constructor(ci) {
    this.kinds = [];
    this.byteVals = [];
    this.nexts = [];
    this.dps = [];
    this.splitBs = [];
    this.clsRefs = [];
    this.tails = [];
    this.ci = ci;
  }

  alloc(kind, byteVal, next, dp, splitB = UNSET, cls = null) {
    if (this.kinds.length >= MAX_SEG_NFA_STATES) return -1;
    this.kinds.push(kind);
    this.byteVals.push(byteVal);
    this.nexts.push(next);
    this.dps.push(dp);
    this.splitBs.push(splitB);
    this.clsRefs.push(cls);
    return this.kinds.length - 1;
  }

  patch(state, target) {
    if (this.kinds[state] === S_SPLIT) {
      if (this.nexts[state] === UNSET) this.nexts[state] = target;
      if (this.splitBs[state] === UNSET) this.splitBs[state] = target;
      return;
    }
    if (this.nexts[state] === UNSET) this.nexts[state] = target;
  }

  compileOps(ops, dot) {
    if (ops.length === 0) {
      const s = this.alloc(S_JUMP, 0, UNSET, 0);
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
        const s = this.alloc(S_ANY, 0, UNSET, dot ? 0 : 1);
        return s === -1 ? null : [s, [s]];
      }
      case OP_CLASS: {
        const dp = !dot && op.cls.neg ? 1 : 0;
        const s = this.alloc(S_CLASS, 0, UNSET, dp, UNSET, op.cls);
        return s === -1 ? null : [s, [s]];
      }
      case OP_STAR: {
        const entry = this.alloc(S_SPLIT, 0, UNSET, 0);
        if (entry === -1) return null;
        const body = this.alloc(S_ANY, 0, entry, dot ? 0 : 1);
        if (body === -1) return null;
        const exit = this.alloc(dot ? S_JUMP : S_DOT_GUARD, 0, UNSET, 0);
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
          const s = this.alloc(S_SPLIT, 0, a, 0, b);
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
    const alt = asciiCaseAlt(b);
    if (this.ci && alt !== b) {
      // Shared makers keep the synthesized class on the same hidden
      // class as parser-produced ones — `classMatches` stays
      // monomorphic in the per-byte loop.
      const cls = klass(false, [classItemByte(b), classItemByte(alt)]);
      return this.alloc(S_CLASS, 0, UNSET, 0, UNSET, cls);
    }
    return this.alloc(S_BYTE, b, UNSET, 0);
  }
}
