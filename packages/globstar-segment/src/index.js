// @globstar/segment — the segment-structured matcher (SSM) as a
// standalone package, benchmarked head-to-head against
// `@globstar/globstar`'s engines and third-party libraries.
//
// Shares the sibling package's parser, lowering, options, error type,
// and DirMatch contract — both packages compile the exact same
// dialect and are held to the same corpus + differential fuzzer.
//
// Public surface mirrors `@globstar/globstar`:
//   globstar(patterns, options?)       → (input) => boolean
//   compileMatcher(patterns, options?) → { match, matchDir, staticPrefixes }
//
// Engine pick: pure literal → LiteralMatcher (string fast path);
// segment-expressible (≈ all real patterns) → SegmentMatcher; the
// remainder → PikeVm. The segment engine consumes JS strings natively
// (zero-copy, no UTF-8 encode); byte inputs work everywhere.

import { parse } from "../../globstar/src/matcher/parser.js";
import { lower } from "../../globstar/src/matcher/engine/ops.js";
import { LiteralMatcher } from "../../globstar/src/matcher/engine/literal.js";
import { PikeVm } from "../../globstar/src/matcher/engine/pikevm.js";
import { nodeToLiteralBytes } from "../../globstar/src/matcher/ast.js";
import { factorBranches } from "../../globstar/src/matcher/factor.js";
import { GlobError } from "../../globstar/src/matcher/error.js";
import { DirMatch } from "../../globstar/src/matcher/dir-match.js";
import { toBytes } from "../../globstar/src/matcher/utf8.js";
import { SegmentMatcher } from "./engine.js";

const DEFAULT_OPTIONS = { dot: true, caseInsensitive: false };

export function globstar(patterns, options) {
  return compileMatcher(patterns, options).match;
}

export function compileMatcher(patterns, options) {
  const opts = options == null ? DEFAULT_OPTIONS : { ...DEFAULT_OPTIONS, ...options };
  const list = Array.isArray(patterns) ? patterns : [patterns];
  if (list.length === 0) throw new GlobError("EmptyPatternSet");

  const positiveBodies = [];
  const negativeBodies = [];
  for (let i = 0; i < list.length; i++) {
    const ast = parse(String(list[i]));
    if (ast.isNegated) negativeBodies.push(ast.body);
    else positiveBodies.push(ast.body);
  }

  const positiveEngine = positiveBodies.length > 0 ? buildEngine(positiveBodies, opts) : null;
  const negativeEngines = negativeBodies.map((body) => buildEngine([body], opts));

  return makeMatcher(positiveEngine, negativeEngines);
}

function buildEngine(bodies, opts) {
  const ci = !!opts.caseInsensitive;
  const dot = !!opts.dot;
  if (bodies.length === 1) {
    const literalBytes = nodeToLiteralBytes(bodies[0]);
    if (literalBytes !== null) return new LiteralMatcher(literalBytes, ci);
  }
  const factored = bodies.length === 1 ? bodies[0] : factorBranches(bodies);
  const program = lower(factored, ci);
  // SSM-first; segment-inexpressible patterns (globstars glued
  // mid-segment via braces, escaped separators, fork/state budget
  // overflows) take the PikeVm.
  const seg = SegmentMatcher.build(program, dot);
  if (seg !== null) return seg;
  return PikeVm.build(program, dot);
}

function makeMatcher(positiveEngine, negativeEngines) {
  const hasNegatives = negativeEngines.length > 0;

  // The segment matcher (and the shared LiteralMatcher) consume JS
  // strings natively; the PikeVm needs one UTF-8 encode — done lazily
  // and at most once per call, without a per-call helper closure.
  const match = (input) => {
    const isStr = typeof input === "string";
    let bytes = null;
    if (positiveEngine !== null) {
      const arg =
        positiveEngine.acceptsStrings || !isStr ? input : (bytes ??= toBytes(input));
      if (positiveEngine.isMatch(arg)) return true;
    }
    for (let i = 0; i < negativeEngines.length; i++) {
      const e = negativeEngines[i];
      const arg = e.acceptsStrings || !isStr ? input : (bytes ??= toBytes(input));
      if (!e.isMatch(arg)) return true; // `!body.match(p) === true`
    }
    return false;
  };

  const matchDir = (input) => {
    if (positiveEngine === null) return DirMatch.Descend;
    const arg =
      positiveEngine.acceptsStrings || typeof input !== "string" ? input : toBytes(input);
    const dm = positiveEngine.matchDir(arg);
    // With any negated branch present, descend pruning is unsafe.
    if (hasNegatives) return DirMatch.isMatch(dm) ? DirMatch.DescendAndMatch : DirMatch.Descend;
    return dm;
  };

  const staticPrefixes = () => (positiveEngine !== null ? positiveEngine.staticPrefixes() : []);

  return { match, matchDir, staticPrefixes };
}
