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
import { lower } from "./engine/ops.js";
import { LiteralMatcher } from "./engine/literal.js";
import { SegmentMatcher } from "./engine/segment.js";
import { PikeVm } from "./engine/pikevm.js";
import { nodeToLiteralBytes } from "./ast.js";
import { factorBranches } from "./factor.js";
import { GlobError } from "./error.js";
import { DirMatch } from "./dir-match.js";
import { toBytes } from "./utf8.js";

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
  if (asts.length === 1) {
    const literalBytes = nodeToLiteralBytes(asts[0].body);
    if (literalBytes !== null) return new LiteralMatcher(literalBytes, ci);
  }
  // factorBranches can move a top-level `**` into the synthetic brace,
  // so multi-pattern merges keep the distribution walk enabled.
  const maybe = asts.length > 1 ? true : asts[0].maybeSepDistribution;
  const factored = asts.length === 1 ? asts[0].body : factorBranches(asts.map((a) => a.body));
  const program = lower(factored, ci, maybe);

  if (opts.__engine === "pikevm") return PikeVm.build(program, dot);
  return SegmentMatcher.build(program, dot) ?? PikeVm.build(program, dot);
}

function makeMatcher(positiveEngine, negativeEngines) {
  const hasNegatives = negativeEngines.length > 0;

  // Segment and literal engines consume strings natively. Fallback/reference
  // engines receive a lazily encoded byte view, at most once per call.
  const match = (input) => {
    const isStr = typeof input === "string";
    let bytes = null;
    if (positiveEngine !== null) {
      const arg = positiveEngine.acceptsStrings || !isStr ? input : (bytes ??= toBytes(input));
      if (positiveEngine.isMatch(arg)) return true;
    }
    for (let i = 0; i < negativeEngines.length; i++) {
      const engine = negativeEngines[i];
      const arg = engine.acceptsStrings || !isStr ? input : (bytes ??= toBytes(input));
      if (!engine.isMatch(arg)) return true; // `!body.match(p) === true`
    }
    return false;
  };

  const matchDir = (input) => {
    if (positiveEngine === null) return DirMatch.Descend;
    const arg = positiveEngine.acceptsStrings || typeof input !== "string" ? input : toBytes(input);
    const dm = positiveEngine.matchDir(arg);
    // With any negated branch present, descend pruning is unsafe (the
    // negation could match arbitrarily deep paths we haven't seen yet).
    // Conservatively force Descend, preserve positive Match flag.
    if (hasNegatives) return DirMatch.isMatch(dm) ? DirMatch.DescendAndMatch : DirMatch.Descend;
    return dm;
  };

  // Negated branches don't contribute — a negation has no useful
  // jump-in point.
  const staticPrefixes = () => (positiveEngine !== null ? positiveEngine.staticPrefixes() : []);

  return { match, matchDir, staticPrefixes };
}
