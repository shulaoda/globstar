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

export function seqMatches(seq, str, dot, ci, bail) {
  if (seq.gCount === 0) return matchFixed(seq, str, ci, bail);
  if (seq.gCount === 1) return matchSingleG(seq, str, dot, ci, bail);
  const active = nfaRun(seq, str, dot, ci, bail);
  if (active === -1) return BAIL;
  return (active & (1 << (seq.numStates - 1))) !== 0 ? YES : NO;
}

function nextSep(str, from) {
  const i = str.indexOf("/", from);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.indexOf("\\", from);
  if (i === -1) return j;
  if (j === -1) return i;
  return i < j ? i : j;
}

function matchFixed(seq, str, ci, bail) {
  const elems = seq.elems;
  const m = elems.length;
  let pos = 0;
  for (let i = 0; i < m; i++) {
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    if (i + 1 < m) {
      if (last) return NO;
    } else if (!last) {
      return NO;
    }
    const r = elemConsumes(elems[i], str, pos, end, ci, bail);
    if (r !== YES) return r;
    pos = end + 1;
  }
  return YES;
}

function matchSingleG(seq, str, dot, ci, bail) {
  const elems = seq.elems;
  const g = seq.singleG;
  const m = elems.length;
  const tailLen = m - g - 1;

  let tailEnd = str.length;
  let ts = 0;
  for (let j = tailLen - 1; j >= 0; j--) {
    let s = lastSepBefore(str, tailEnd);
    s = s === -1 ? 0 : s + 1;
    const r = elemConsumes(elems[g + 1 + j], str, s, tailEnd, ci, bail);
    if (r !== YES) return r;
    if (j > 0) {
      if (s === 0) return NO;
      tailEnd = s - 1;
    }
    ts = s;
  }

  let midStart;
  const jh = seq.joinedHeadStr;
  if (jh !== null) {
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
      if (pos > str.length) return NO;
      let end = nextSep(str, pos);
      if (end === -1) end = str.length;
      const r = elemConsumes(elems[i], str, pos, end, ci, bail);
      if (r !== YES) return r;
      pos = end + 1;
    }
    midStart = pos;
  }

  let midExists;
  let midEnd;
  if (tailLen > 0) {
    if (ts < midStart) return NO;
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
  if (midStart < midEnd && str.charCodeAt(midStart) === 0x2e) return NO;
  for (let i = midStart; ; ) {
    i = nextSep(str, i);
    if (i === -1 || i + 1 >= midEnd) return YES;
    if (str.charCodeAt(i + 1) === 0x2e) return NO;
    i += 1;
  }
}

function lastSepBefore(str, end) {
  if (end <= 0) return -1;
  const i = str.lastIndexOf("/", end - 1);
  if (!IS_WINDOWS_SEP) return i;
  const j = str.lastIndexOf("\\", end - 1);
  return i > j ? i : j;
}

export function nfaRun(seq, str, dot, ci, bail) {
  let active = seq.eps[seq.stateOf[0]];
  let pos = 0;
  for (;;) {
    if (active === 0) return 0;
    let end = nextSep(str, pos);
    const last = end === -1;
    if (last) end = str.length;
    active = nfaStep(seq, active, str, pos, end, dot, ci, bail);
    if (active === -1) return -1;
    if (last) return active;
    pos = end + 1;
  }
}

function nfaStep(seq, active, str, s0, e0, dot, ci, bail) {
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
        const r = litEq(e.litStr, str, s0, e0, ci);
        if (r === YES) next |= eps[nextEntry];
        break;
      }
      case EL_WILD: {
        const r = wildConsumes(e.wild, str, s0, e0, ci, bail);
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

function elemConsumes(e, str, s, t, ci, bail) {
  if (e.kind === EL_LIT) return litEq(e.litStr, str, s, t, ci);
  if (e.kind === EL_WILD) return wildConsumes(e.wild, str, s, t, ci, bail);
  return NO;
}

function litEq(lit, str, s, t, ci) {
  if (t - s !== lit.length) return NO;
  if (!ci) return str.startsWith(lit, s) ? YES : NO;
  for (let i = 0; i < lit.length; i++) {
    if (!eqByteCi(lit.charCodeAt(i), str.charCodeAt(s + i))) return NO;
  }
  return YES;
}

function affixEq(part, str, at, ci) {
  if (!ci) return str.startsWith(part, at) ? YES : NO;
  for (let i = 0; i < part.length; i++) {
    if (!eqByteCi(part.charCodeAt(i), str.charCodeAt(at + i))) return NO;
  }
  return YES;
}

function segHasNonAscii(str, s, t) {
  for (let i = s; i < t; i++) {
    if (str.charCodeAt(i) > 0x7f) return true;
  }
  return false;
}

function wildConsumes(w, str, s, t, ci, bail) {
  if (w.dotProtect && t > s && str.charCodeAt(s) === 0x2e) return NO;
  const len = t - s;
  switch (w.kind) {
    case WK_AFFIX: {
      if (bail && w.anychars > 0 && segHasNonAscii(str, s, t)) return BAIL;
      const need = w.minLen;
      if (len < need || (!w.variable && len !== need)) return NO;
      if (w.prefixStr.length > 0 && affixEq(w.prefixStr, str, s, ci) === NO) return NO;
      if (w.suffixStr.length > 0 && affixEq(w.suffixStr, str, t - w.suffixStr.length, ci) === NO) {
        return NO;
      }
      return YES;
    }
    case WK_AFFIX_SET: {
      if (bail && w.anychars > 0 && segHasNonAscii(str, s, t)) return BAIL;
      const p = w.prefixStr;
      if (len < p.length || (p.length > 0 && affixEq(p, str, s, ci) === NO)) return NO;
      const set = w.suffixSetStr;
      for (let i = 0; i < set.length; i++) {
        const suf = set[i];
        const need = w.minLen + suf.length;
        if (len < need || (!w.variable && len !== need)) continue;
        if (suf.length === 0 || affixEq(suf, str, t - suf.length, ci) !== NO) return YES;
      }
      return NO;
    }
    default: {
      const nfa = w.nfa;
      if (bail && nfa.needsAsciiSeg && segHasNonAscii(str, s, t)) return BAIL;
      return nfa.matches(str, s, t) ? YES : NO;
    }
  }
}

export function endsWithSepAware(str, suffix, ci) {
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
