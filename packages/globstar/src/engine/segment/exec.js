// Segment-at-a-time matching over a compiled `ElemSeq`. JS port of the
// Rust module `crates/globstar/src/engine/segment/exec.rs`, carrying
// BOTH a string-mode path (UTF-16 fast path) and a byte-mode path.

import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_G0_STRICT,
  EL_G1,
  WK_AFFIX,
  WK_AFFIX_SET,
  NO,
  YES,
  BAIL,
} from "./index.js";
import { isPathSep, eqByteCi, IS_WINDOWS_SEP, ctz32 } from "../../options.js";

export function acceptBit(seq) {
  return 1 << (seq.numStates - 1);
}

// ---------------------------------------------------------------------------
// Matching — string mode
// ---------------------------------------------------------------------------

export function seqMatchesStr(seq, str, dot, ci) {
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
    // `pos` lands on `len + 1` after the final segment, a sentinel no
    // real segment start can equal (mirrors Rust SegIter).
    let pos = 0;
    for (let i = 0; i < g; i++) {
      if (pos > str.length) return NO;
      let end = nextSepStr(str, pos);
      if (end === -1) end = str.length;
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
    midExists = midStart <= str.length;
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
  // `lastIndexOf` clamps a negative position to 0 instead of returning -1,
  // which would report a separator AT index 0 when there is none before it.
  if (end <= 0) return -1;
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

export function nfaRunStr(seq, str, dot, ci) {
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
export function endsWithSepAwareStr(str, suffix, ci) {
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

export function seqMatchesBytes(seq, bytes, dot, ci) {
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

  // `pos` lands on `len + 1` after the final segment, a sentinel no
  // real segment start can equal (mirrors Rust SegIter).
  let pos = 0;
  for (let i = 0; i < g; i++) {
    if (pos > bytes.length) return false;
    let end = nextSepBytes(bytes, pos);
    if (end === -1) end = bytes.length;
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
    midExists = midStart <= bytes.length;
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

export function nfaRunBytes(seq, bytes, dot, ci) {
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
        if (litEqBytes(e.litStr, bytes, s0, e0, ci)) next |= eps[nextEntry];
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
  if (e.kind === EL_LIT) return litEqBytes(e.litStr, bytes, s, t, ci);
  if (e.kind === EL_WILD) return wildConsumesBytes(e.wild, bytes, s, t, ci);
  return false;
}

function litEqBytes(lit, bytes, s, t, ci) {
  return t - s === lit.length && affixEqBytes(lit, bytes, s, ci);
}

export function affixEqBytes(part, bytes, at, ci) {
  for (let i = 0; i < part.length; i++) {
    const a = part.charCodeAt(i);
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
      if (w.prefixStr.length > 0 && !affixEqBytes(w.prefixStr, bytes, s, ci)) return false;
      if (w.suffixStr.length > 0 && !affixEqBytes(w.suffixStr, bytes, t - w.suffixStr.length, ci)) {
        return false;
      }
      return true;
    }
    case WK_AFFIX_SET: {
      const p = w.prefixStr;
      if (len < p.length || (p.length > 0 && !affixEqBytes(p, bytes, s, ci))) return false;
      const set = w.suffixSetStr;
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
