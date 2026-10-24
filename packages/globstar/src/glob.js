import { parse } from "./parse/parser.js";
import { LiteralMatcher } from "./engine/literal.js";
import { SegmentMatcher } from "./engine/segment/index.js";
import { nodeToLiteral } from "./parse/ast.js";
import { factorBranches } from "./union.js";
import { GlobError } from "./error.js";
import { DirMatch } from "./dir-match.js";

// Only own keys count, `undefined` keeps the default. `__engine` is a test
// hook: `"nfa"` leaves the anchored chains out, `"bytes"` the string mode.
const MATCHER_DEFAULTS = { dot: true, caseInsensitive: false, __engine: undefined };

export function globstar(patterns, options) {
  return compileMatcher(patterns, options).match;
}

export function compileMatcher(patterns, options) {
  const opts = matcherOptions(options);
  const isList =
    typeof patterns !== "string" &&
    patterns != null &&
    typeof patterns[Symbol.iterator] === "function";
  const given = isList ? [...patterns] : [patterns];
  if (given.length === 0) throw new GlobError("EmptyPatternSet");

  const positiveAsts = [];
  const negativeAsts = [];
  const seen = given.length > 1 ? new Set() : null;
  for (let i = 0; i < given.length; i++) {
    const pattern = given[i];
    if (typeof pattern !== "string") {
      throw new TypeError(
        `pattern must be a string, got ${pattern === null ? "null" : typeof pattern}`,
      );
    }
    // A lone surrogate has no UTF-8 form (GLOB_SPEC §2.1).
    if (!pattern.isWellFormed()) {
      throw new TypeError(
        `pattern must be well-formed UTF-16, got a lone surrogate at index ${pattern.search(/\p{Cs}/u)}`,
      );
    }
    if (seen !== null) {
      if (seen.has(pattern)) continue;
      seen.add(pattern);
    }
    let ast;
    try {
      ast = parse(pattern);
    } catch (e) {
      if (isList && e instanceof GlobError) throw inMember(e, i);
      throw e;
    }
    if (ast.isNegated) negativeAsts.push(ast);
    else positiveAsts.push(ast);
  }

  const positiveEngine = positiveAsts.length > 0 ? buildEngine(positiveAsts, opts) : null;
  const negativeEngines = negativeAsts.map((ast) => buildEngine([ast], opts));
  return makeMatcher(positiveEngine, negativeEngines);
}

function inMember(e, index) {
  e.index = index;
  e.message = `pattern ${index}: ${e.message}`;
  return e;
}

function matcherOptions(input) {
  if (input === undefined || input === null) return { ...MATCHER_DEFAULTS };
  if (typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError(
      `options must be an object, got ${Array.isArray(input) ? "an array" : typeof input}`,
    );
  }
  for (const key of Object.keys(input)) {
    if (!Object.hasOwn(MATCHER_DEFAULTS, key)) throw new TypeError(`unknown option "${key}"`);
  }
  const flag = (key) => {
    const v =
      Object.hasOwn(input, key) && input[key] !== undefined ? input[key] : MATCHER_DEFAULTS[key];
    if (typeof v !== "boolean") {
      throw new TypeError(`${key} must be a boolean, got ${v === null ? "null" : typeof v}`);
    }
    return v;
  };
  const engine = Object.hasOwn(input, "__engine") ? input.__engine : undefined;
  if (engine !== undefined && engine !== "nfa" && engine !== "bytes") {
    throw new TypeError(`__engine must be "nfa" or "bytes", got ${String(engine)}`);
  }
  return { dot: flag("dot"), caseInsensitive: flag("caseInsensitive"), __engine: engine };
}

// Exported for the tests, which read how a pattern compiled.
export function buildEngine(asts, opts) {
  const body = asts.length === 1 ? asts[0].body : factorBranches(asts.map((a) => a.body));
  const ascii = asts.every((a) => a.ascii);
  const literal = nodeToLiteral(body);
  const ci = opts.caseInsensitive;
  if (literal !== null) return new LiteralMatcher(literal, ci, ascii);
  return new SegmentMatcher(body, opts.dot, ci, {
    forceNfa: opts.__engine === "nfa",
    byteOnly: opts.__engine === "bytes" || !ascii,
  });
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

  // Anything below may match a negated member (GLOB_SPEC §15.4).
  const matchDir = (input) => {
    requireStringInput(input);
    if (!hasNegatives) return positiveEngine.matchDir(input);
    const below = positiveEngine === null ? DirMatch.Descend : positiveEngine.matchDir(input);
    return DirMatch.from(match(input), true, DirMatch.matchesAllBelow(below));
  };

  const staticPrefixes = () =>
    hasNegatives ? [new Uint8Array(0)] : positiveEngine.staticPrefixes();

  return { match, matchDir, staticPrefixes };
}
