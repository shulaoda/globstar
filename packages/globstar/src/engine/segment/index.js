// SSM — segment-structured matcher. JS port of the Rust module
// `crates/globstar/src/engine/segment/` (its `mod.rs`); see
// `references/decisions/segment-engine-design.md`.
//
// One algorithm, one subject type — a JS string — in two forms:
//
// - **UTF-16 form** (default): the caller's own string, matched with
//   `charCodeAt` / `startsWith` / `endsWith` / `indexOf` intrinsics —
//   zero per-call allocation, no UTF-8 encode. The only two constructs
//   whose semantics depend on *counting* (`?`, one BYTE; negated
//   classes) BAIL when they would touch a char > 0x7F.
// - **Latin-1 form**: `utf8Latin1(input)` renders the UTF-8 bytes one
//   char per byte, so counting is exact and nothing bails. Entered on a
//   BAIL, or straight away when the pattern itself is non-ASCII.
//
// Patterns the segment model cannot express return `null` from `build`
// and the caller falls back to the PikeVm.

import { computeStaticPrefixes } from "../ops/index.js";
import { IS_WINDOWS_SEP } from "../../options.js";
import { latin1Bytes, utf8Latin1 } from "../../utf8.js";
import { DirMatch } from "../../dir-match.js";
import { compileSeqs, opsHaveNonAscii } from "./compile.js";
import { seqMatches, nfaRun, endsWithSepAware } from "./exec.js";

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

// Tri-state match results (`BAIL` ⇒ retry on the Latin-1 form).
export const NO = 0;
export const YES = 1;
export const BAIL = 2;

export class SegmentMatcher {
  constructor(seqs, program, byteOnly, dot) {
    this.seqs = seqs;
    this.ci = !!program.caseInsensitive;
    this.dot = dot;
    this.byteOnly = byteOnly;
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
    this.factsSuffixStr = f.suffix.length > 0 ? f.suffix : null;
    this.factsSuffixSetStr = f.suffixSet.length > 0 ? f.suffixSet : null;
  }

  // `null` ⇒ not segment-expressible; caller falls back.
  static build(program, dot) {
    const seqs = compileSeqs(program.ops, dot, !!program.caseInsensitive);
    if (seqs === null) return null;
    return new SegmentMatcher(seqs, program, opsHaveNonAscii(program.ops), dot);
  }

  staticPrefixes() {
    // Prefixes live as Latin-1 strings; the walker contract is bytes.
    return this.prefixes.map(latin1Bytes);
  }

  isMatch(input) {
    if (!this.byteOnly) {
      const r = this._isMatch(input, true);
      if (r !== BAIL) return r === YES;
    }
    return this._isMatch(utf8Latin1(input), false) === YES;
  }

  matchDir(input) {
    // Empty dir path is the cwd and every match lives under it, so descent
    // is always on.
    if (input.length === 0) return DirMatch.fromExactPrefix(this.isMatch(input), true);
    if (!this.byteOnly) {
      const r = this._matchDir(input, true);
      if (r !== -1) return r;
    }
    return this._matchDir(utf8Latin1(input), false);
  }

  // `BAIL` ⇒ re-run on the Latin-1 form.
  _isMatch(str, bail) {
    if (!this._factsAccept(str)) return NO;
    const seqs = this.seqs;
    // Fork-local suffix prefilter (multi-fork only; skipped under ci
    // — it is an optimization, the full match re-checks everything).
    const quick = seqs.length > 1 && !this.ci;
    let bailed = false;
    for (let i = 0; i < seqs.length; i++) {
      const seq = seqs[i];
      if (quick && seq.quickSuffixStr.length > 0 && !str.endsWith(seq.quickSuffixStr)) continue;
      const r = seqMatches(seq, str, this.dot, this.ci, bail);
      if (r === YES) return YES;
      if (r === BAIL) bailed = true;
    }
    return bailed ? BAIL : NO;
  }

  _factsAccept(str) {
    const plain = this.factsPlain;
    const suf = this.factsSuffixStr;
    if (suf !== null) {
      return plain ? str.endsWith(suf) : endsWithSepAware(str, suf, this.ci);
    }
    const set = this.factsSuffixSetStr;
    if (set !== null) {
      for (let i = 0; i < set.length; i++) {
        if (plain ? str.endsWith(set[i]) : endsWithSepAware(str, set[i], this.ci)) return true;
      }
      return false;
    }
    return true;
  }

  // -1 ⇒ re-run on the Latin-1 form.
  _matchDir(str, bail) {
    let exact = false;
    let prefix = false;
    const seqs = this.seqs;
    for (let i = 0; i < seqs.length; i++) {
      const active = nfaRun(seqs[i], str, this.dot, this.ci, bail);
      if (active === -1) return -1;
      if ((active & (1 << (seqs[i].numStates - 1))) !== 0) exact = true;
      if ((active & seqs[i].reach1) !== 0) prefix = true;
      if (exact && prefix) break;
    }
    return DirMatch.fromExactPrefix(exact, prefix);
  }
}
