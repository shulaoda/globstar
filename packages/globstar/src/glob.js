import { parse } from "./parser.js";
import { lower } from "./engine/ops/index.js";
import { LiteralMatcher } from "./engine/literal.js";
import { SegmentMatcher } from "./engine/segment/index.js";
import { PikeVm } from "./engine/pikevm.js";
import { nodeToLiteralBytes } from "./ast.js";
import { factorBranches } from "./factor.js";
import { GlobError } from "./error.js";
import { DirMatch } from "./dir-match.js";

export function globstar(patterns, options) {
  return compileMatcher(patterns, options).match;
}

export function compileMatcher(patterns, options) {
  // Only an absent or `undefined` value means "default".
  const o = options ?? {};
  const opts = {
    dot: o.dot === undefined ? true : o.dot,
    caseInsensitive: o.caseInsensitive === undefined ? false : o.caseInsensitive,
    // Internal test hook; own-property read defeats prototype pollution.
    __engine: Object.hasOwn(o, "__engine") ? o.__engine : undefined,
  };
  const list = Array.isArray(patterns) ? patterns : [patterns];
  if (list.length === 0) throw new GlobError("EmptyPatternSet");

  const positiveAsts = [];
  const negativeAsts = [];
  for (let i = 0; i < list.length; i++) {
    const pattern = list[i];
    if (typeof pattern !== "string") {
      throw new TypeError(
        `pattern must be a string, got ${pattern === null ? "null" : typeof pattern}`,
      );
    }
    const ast = parse(pattern);
    if (ast.isNegated) negativeAsts.push(ast);
    else positiveAsts.push(ast);
  }

  const positiveEngine = positiveAsts.length > 0 ? buildEngine(positiveAsts, opts) : null;
  const negativeEngines = negativeAsts.map((ast) => buildEngine([ast], opts));

  return makeMatcher(positiveEngine, negativeEngines);
}

function buildEngine(asts, opts) {
  const ci = !!opts.caseInsensitive;
  const dot = !!opts.dot;
  const forcePikevm = opts.__engine === "pikevm";
  if (!forcePikevm && asts.length === 1) {
    const literalBytes = nodeToLiteralBytes(asts[0].body);
    if (literalBytes !== null) return new LiteralMatcher(literalBytes, ci);
  }
  const factored = asts.length === 1 ? asts[0].body : factorBranches(asts.map((a) => a.body));
  const program = lower(factored, ci);

  if (forcePikevm) return PikeVm.build(program, dot);
  return SegmentMatcher.build(program, dot) ?? PikeVm.build(program, dot);
}

function requireStringInput(input) {
  if (typeof input !== "string") {
    throw new TypeError(`path must be a string, got ${input === null ? "null" : typeof input}`);
  }
}

function makeMatcher(positiveEngine, negativeEngines) {
  const hasNegatives = negativeEngines.length > 0;

  const match = (input) => {
    requireStringInput(input);
    if (positiveEngine !== null && positiveEngine.isMatch(input)) return true;
    for (let i = 0; i < negativeEngines.length; i++) {
      if (!negativeEngines[i].isMatch(input)) return true;
    }
    return false;
  };

  const matchDir = (input) => {
    requireStringInput(input);
    if (positiveEngine === null) return DirMatch.Descend;
    const dm = positiveEngine.matchDir(input);
    if (hasNegatives) return DirMatch.isMatch(dm) ? DirMatch.DescendAndMatch : DirMatch.Descend;
    return dm;
  };

  const staticPrefixes = () =>
    hasNegatives ? [new Uint8Array(0)] : positiveEngine.staticPrefixes();

  return { match, matchDir, staticPrefixes };
}
