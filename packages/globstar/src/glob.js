// Matcher factory.
//
// Public: globstar(patterns, options?)       → (input) => boolean
//         compileMatcher(patterns, options?) → { match, matchDir, staticPrefixes }
//
// Both are re-exported from `src/index.js`. `compileMatcher` is the
// walker-facing surface (`@globstar/walk` consumes `matchDir` /
// `staticPrefixes`), mirroring the Rust crate's `Glob` methods.
//
// Multi-pattern combines via OR. Each pattern's own `!`-prefix
// applies independently; auto-splitting `!` into ignores is the
// walker's job, not the matcher's. By the time walker calls us, all
// patterns it passes are strictly positive.
//
// Engine pick (in order):
//   pure literal pattern       → LiteralMatcher
//   segment-expressible shape  → SegmentMatcher
//   bounded/shape overflow     → PikeVm
//
// `__engine: "pikevm"` remains an internal verification escape hatch;
// production callers and the walker use SSM.

import { parse } from "./parser.js";
import { lower } from "./engine/ops/index.js";
import { LiteralMatcher } from "./engine/literal.js";
import { SegmentMatcher } from "./engine/segment/index.js";
import { PikeVm } from "./engine/pikevm.js";
import { nodeToLiteralBytes } from "./ast.js";
import { factorBranches } from "./factor.js";
import { GlobError } from "./error.js";
import { DirMatch } from "./dir-match.js";

const DEFAULT_OPTIONS = { dot: true, caseInsensitive: false };

export function globstar(patterns, options) {
  return compileMatcher(patterns, options).match;
}

// Note: unlike Rust `Glob::union` (a pure OR of positive patterns,
// which rejects any `!`-prefixed input), this factory deliberately
// accepts negated patterns — include/exclude is part of the JS
// package's public contract.
export function compileMatcher(patterns, options) {
  const opts = options == null ? DEFAULT_OPTIONS : { ...DEFAULT_OPTIONS, ...options };
  const list = Array.isArray(patterns) ? patterns : [patterns];
  if (list.length === 0) throw new GlobError("EmptyPatternSet");

  const positiveAsts = [];
  const negativeAsts = [];
  for (let i = 0; i < list.length; i++) {
    const ast = parse(String(list[i]));
    if (ast.isNegated) negativeAsts.push(ast);
    else positiveAsts.push(ast);
  }

  // Positive branches collapse into one engine via `factorBranches`
  // (shared prefix/suffix → smaller segment program or fallback NFA).
  const positiveEngine = positiveAsts.length > 0 ? buildEngine(positiveAsts, opts) : null;
  // Negative branches stay as N independent engines, each contributing
  // `!body.match(input)` to the OR. Rare path; not worth factoring.
  const negativeEngines = negativeAsts.map((ast) => buildEngine([ast], opts));

  return makeMatcher(positiveEngine, negativeEngines);
}

function buildEngine(asts, opts) {
  const ci = !!opts.caseInsensitive;
  const dot = !!opts.dot;
  const forcePikevm = opts.__engine === "pikevm";
  // The tier-0 literal shortcut would also swallow forced-pikevm builds, so
  // it only applies when no engine is forced.
  if (!forcePikevm && asts.length === 1) {
    const literalBytes = nodeToLiteralBytes(asts[0].body);
    if (literalBytes !== null) return new LiteralMatcher(literalBytes, ci);
  }
  const factored = asts.length === 1 ? asts[0].body : factorBranches(asts.map((a) => a.body));
  const program = lower(factored, ci);

  if (forcePikevm) return PikeVm.build(program, dot);
  return SegmentMatcher.build(program, dot) ?? PikeVm.build(program, dot);
}

function makeMatcher(positiveEngine, negativeEngines) {
  const hasNegatives = negativeEngines.length > 0;

  const match = (input) => {
    if (positiveEngine !== null && positiveEngine.isMatch(input)) return true;
    for (let i = 0; i < negativeEngines.length; i++) {
      if (!negativeEngines[i].isMatch(input)) return true; // `!body.match(p) === true`
    }
    return false;
  };

  const matchDir = (input) => {
    if (positiveEngine === null) return DirMatch.Descend;
    const dm = positiveEngine.matchDir(input);
    // With any negated branch present, descend pruning is unsafe (the
    // negation could match arbitrarily deep paths we haven't seen yet).
    // Conservatively force Descend, preserve positive Match flag.
    if (hasNegatives) return DirMatch.isMatch(dm) ? DirMatch.DescendAndMatch : DirMatch.Descend;
    return dm;
  };

  // With a negated branch, matches can fall anywhere the negation
  // rejects, so seed from the cwd (a single empty prefix, never an
  // empty list — the Rust twin's negated convention). No positive
  // patterns implies a negative one, so `hasNegatives` covers the
  // `positiveEngine === null` case too.
  const staticPrefixes = () =>
    hasNegatives ? [new Uint8Array(0)] : positiveEngine.staticPrefixes();

  return { match, matchDir, staticPrefixes };
}
