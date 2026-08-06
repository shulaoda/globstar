// SSM — segment-structured matcher. JS port of the Rust module
// `crates/globstar/src/engine/segment/` (its `mod.rs`); see
// `references/decisions/segment-engine-design.md`.
//
// One algorithm, two execution modes:
//
// - **String mode** (default): matches directly on the JS string with
//   `charCodeAt` / `startsWith` / `endsWith` / `indexOf` intrinsics —
//   zero per-call allocation, no UTF-8 encode. The only two constructs
//   whose semantics depend on *counting* (`?`, one BYTE; negated
//   classes) BAIL to byte mode when they would touch a char > 0x7F.
// - **Byte mode**: `toBytes(input)` once, same algorithm over the
//   `Uint8Array`. Also used for `Uint8Array` inputs.
//
// Patterns the segment model cannot express return `null` from `build`
// and the caller falls back to the PikeVm.

import { computeStaticPrefixes } from "../ops/index.js";
import { IS_WINDOWS_SEP } from "../../options.js";
import { toBytes, latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import {
  expandForks,
  opsHaveNonAscii,
  hasOpenGlobstarAdjacency,
  collapseOpenGlobstars,
  segmentize,
} from "./compile.js";
import {
  seqMatchesStr,
  seqMatchesBytes,
  nfaRunStr,
  nfaRunBytes,
  acceptBit,
  endsWithSepAwareStr,
  affixEqBytes,
} from "./exec.js";

// Fork / element-NFA budgets (masks are 32-bit here; Rust uses 64 —
// overflow just takes the PikeVM fallback, with identical results).
export const MAX_FORKS = 64;
export const MAX_SEQ_STATES = 32;

// Element kinds.
export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2; // absorb >= 0 segments
export const EL_G0_STRICT = 3; // absorb >= 0, first absorbed segment nonempty
export const EL_G1 = 4; // absorb >= 1 segment

// Wild kinds.
export const WK_AFFIX = 0;
export const WK_AFFIX_SET = 1;
export const WK_GENERIC = 2;

// Tri-state results for string-mode matchers.
export const NO = 0;
export const YES = 1;
export const BAIL = 2;

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
    this.factsSuffixSetStr = f.suffixSet.length > 0 ? f.suffixSet.map(latin1) : null;
  }

  /// `null` ⇒ not segment-expressible; caller falls back.
  static build(program, dot) {
    const opSeqs = expandForks(program.ops);
    if (opSeqs === null) return null;
    // Fork expansion introduces no new bytes — one scan of the
    // original ops decides the mode.
    const byteOnly = opsHaveNonAscii(program.ops);
    const seqs = [];
    for (let ops of opSeqs) {
      // Collapse open-globstar adjacencies fork-splicing / separator
      // distribution can create, before segmentizing (ports
      // `segmentize_fork` in engine/compile.rs). Copy first — the
      // no-crossing path returns `program.ops` verbatim.
      if (hasOpenGlobstarAdjacency(ops)) ops = collapseOpenGlobstars(ops.slice());
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
