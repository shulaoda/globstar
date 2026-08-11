// Ops → element sequences: fork expansion, the segmentizer, and
// in-segment wildcard classification. JS port of the Rust module
// `crates/globstar/src/engine/segment/compile.rs`.

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
  SLASH_ANY_OP,
} from "../ops/index.js";
import { CI_BYTE } from "../../ast.js";
import { isPathSep } from "../../options.js";
import { latin1 } from "../../utf8.js";
import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_G0_STRICT,
  EL_G1,
  WK_AFFIX,
  WK_AFFIX_SET,
  WK_GENERIC,
  MAX_FORKS,
  MAX_SEQ_STATES,
} from "./index.js";
import { SegNfa } from "./seg-nfa.js";

const MAX_SUFFIX_PRODUCT = 16;

function makeElem(kind, litBytes, wild) {
  return { kind, litStr: litBytes !== null ? latin1(litBytes) : null, wild };
}

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
  return op.kind === OP_ALTERNATION && opCrossesSegment(op);
}

function hasOpenGlobstarAdjacency(ops) {
  for (let i = 1; i < ops.length; i++) {
    if (
      ops[i].kind === OP_GLOBSTAR_ANY &&
      (ops[i - 1].kind === OP_OPT_SEGMENTS_SLASH || ops[i - 1].kind === OP_SEP_RUN)
    ) {
      return true;
    }
  }
  return false;
}

// `(?:[^/]*/)* .*` = `.*` → GlobstarAny; `/+ .*` = `/.*` → SlashAnything.
// Both language-preserving. Without them the segmentizer turns the `**`
// fork of `x/{**,a}/**` (`SepRun OSS GlobstarAny`) into `[Lit, G0, G0]`,
// dropping the mandatory separator and matching `x` (§8.3). Mutates and
// returns `ops` (caller passes a copy).
function collapseOpenGlobstars(ops) {
  let i = 0;
  while (i < ops.length) {
    if (i > 0 && ops[i].kind === OP_GLOBSTAR_ANY) {
      const prev = ops[i - 1].kind;
      if (prev === OP_OPT_SEGMENTS_SLASH) {
        ops.splice(i - 1, 1); // drop OSS, re-examine GlobstarAny
        i -= 1;
        continue;
      }
      if (prev === OP_SEP_RUN) {
        ops[i - 1] = SLASH_ANY_OP;
        ops.splice(i, 1);
        continue;
      }
    }
    i += 1;
  }
  return ops;
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

export function opsHaveNonAscii(ops) {
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
// An absorber whose op form does not self-delimit (GlobstarAny,
// SlashAnything) was just pushed as the last element. Only the Sep
// that closes it may follow.
const B_OPEN = 3;

const EMPTY_BYTES = new Uint8Array(0);

// Compile the lowered ops into fork sequences. `null` means not
// segment-expressible, so the caller falls back to the PikeVm (ports
// `compile_seqs` + `segmentize_fork` in engine/segment/compile.rs).
export function compileSeqs(ops, dot, ci) {
  const opSeqs = expandForks(ops);
  if (opSeqs === null) return null;
  const seqs = [];
  for (let fork of opSeqs) {
    // Collapse open-globstar adjacencies fork-splicing / separator
    // distribution can create, before segmentizing. Copy first — the
    // no-crossing path returns the caller's ops verbatim.
    if (hasOpenGlobstarAdjacency(fork)) fork = collapseOpenGlobstars(fork.slice());
    const seq = segmentize(fork, dot, ci);
    if (seq === null) return null;
    seqs.push(seq);
  }
  return seqs;
}

function segmentize(ops, dot, ci) {
  const elems = [];
  let buf = [];
  let state = B_FRESH;
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
    // The only op that may follow an open absorber is the separator
    // closing its right edge, and it upgrades the lenient `.*` to "at
    // least one segment". A G1 absorber (SlashAnything, or GlobstarAny
    // behind a strict Sep) is never followed by a Sep after lowering;
    // bail rather than drop the separator.
    if (state === B_OPEN) {
      if (op.kind !== OP_SEP || elems[elems.length - 1]?.kind !== EL_G0) return null;
      elems[elems.length - 1] = makeElem(EL_G1, null, null);
      state = B_FRESH;
      continue;
    }
    switch (op.kind) {
      case OP_LIT:
      case OP_ANYCHAR:
      case OP_STAR:
      case OP_CLASS:
      case OP_ALTERNATION: {
        if (litContainsSep(op)) return null; // escaped separator
        pushInSeg(buf, op);
        break;
      }
      case OP_SEP: {
        const e = closeSegment();
        if (e === null) return null;
        elems.push(e);
        state = B_STRICT;
        break;
      }
      case OP_SEP_RUN: {
        // Generated only immediately before an OSS.
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
        // A glued absorber cannot be produced today (the parser degrades
        // any `**` that does not own a whole segment, §8.1). Defensive
        // bail, PikeVm answers correctly if one ever appears.
        if (buf.length > 0) return null;
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
        // Trailing `/**`: brings its own leading boundary.
        const e = closeSegment();
        if (e === null) return null;
        elems.push(e);
        elems.push(makeElem(EL_G1, null, null));
        state = B_OPEN;
        break;
      }
      case OP_GLOBSTAR_ANY: {
        // Defensive bail, same as the OSS arm above.
        if (buf.length > 0) return null;
        // Behind a strict separator the absorber must consume >= 1
        // segment (`a/{**,x}` rejects `a`).
        const strict = state === B_STRICT;
        elems.push(makeElem(strict ? EL_G1 : EL_G0, null, null));
        state = B_OPEN;
        break;
      }
      default:
        return null; // raw OP_GLOBSTAR never survives the fold
    }
  }

  if (state !== B_OPEN) {
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
    prefixStr: fields.prefixStr ?? "",
    suffixStr: fields.suffixStr ?? "",
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
        prefixStr: latin1(prefix),
        suffixStr: latin1(suffixes[0]),
        minLen: prefix.length + suffixes[0].length + anychars,
        variable: hasStar,
        dotProtect,
        anychars,
      });
    }
    return makeWild(WK_AFFIX_SET, {
      prefixStr: latin1(prefix),
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
  const elemOf = new Array(n).fill(0);
  for (let i = 0; i < m; i++) {
    const end = i + 1 < m ? stateOf[i + 1] : accept;
    for (let s = stateOf[i]; s < end; s++) elemOf[s] = i;
  }

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

  // Pre-join all-literal heads for the single-globstar fast path (mirrors
  // Rust `finish`). "src/**/.." heads become one prefix compare instead of a
  // segment loop. String form only. Byte mode is rare and keeps the loop.
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
  let quickStr = "";
  const lastEl = elems[m - 1];
  if (lastEl.kind === EL_LIT) quickStr = lastEl.litStr;
  else if (lastEl.kind === EL_WILD && lastEl.wild.kind === WK_AFFIX) {
    quickStr = lastEl.wild.suffixStr;
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
    quickSuffixStr: quickStr,
    joinedHeadStr,
  };
}

function elemSatisfiable(e) {
  if (e.kind !== EL_WILD) return true;
  const w = e.wild;
  return w.kind === WK_GENERIC ? w.nfa.satisfiable : true;
}
