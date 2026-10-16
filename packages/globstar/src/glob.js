import { parse } from "./parser.js";
import { lower } from "./engine/ops.js";
import { LiteralMatcher } from "./engine/literal.js";
import { SegmentMatcher } from "./engine/segment/index.js";
import { nodeToLiteralBytes } from "./ast.js";
import { factorBranches } from "./factor.js";
import { GlobError, MAX_EXPANSION } from "./error.js";
import { DirMatch } from "./dir-match.js";

export function globstar(patterns, options) {
  return compileMatcher(patterns, options).match;
}

export function compileMatcher(patterns, options) {
  const opts = matcherOptions(options);
  // A string is one pattern; any other iterable is a list, whose member
  // errors carry the member's `index` (counting every member given).
  const isList =
    typeof patterns !== "string" &&
    patterns != null &&
    typeof patterns[Symbol.iterator] === "function";
  const given = isList ? [...patterns] : [patterns];
  if (given.length === 0) throw new GlobError("EmptyPatternSet");

  const positiveAsts = [];
  const negativeAsts = [];
  const budget = { left: MAX_EXPANSION };
  // The same pattern twice adds nothing to the union: it is parsed once
  // and counts once against the expansion budget.
  const seen = given.length > 1 ? new Set() : null;
  for (let i = 0; i < given.length; i++) {
    const pattern = given[i];
    if (typeof pattern !== "string") {
      throw new TypeError(
        `pattern must be a string, got ${pattern === null ? "null" : typeof pattern}`,
      );
    }
    if (seen !== null) {
      if (seen.has(pattern)) continue;
      seen.add(pattern);
    }
    let ast;
    try {
      ast = parse(pattern, budget);
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

// A list member's error: its `index`, and the message saying which.
function inMember(e, index) {
  e.index = index;
  e.message = `pattern ${index}: ${e.message}`;
  return e;
}

// The options bag with its defaults: only own keys count (a key on
// `Object.prototype` changes nothing), `undefined` keeps the default, an
// unknown key or a non-boolean flag is a TypeError. `__engine` is an
// internal test hook (`"nfa"`: no anchored chains; `"bytes"`: no string
// mode).
const MATCHER_DEFAULTS = { dot: true, caseInsensitive: false, __engine: undefined };
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
  return {
    dot: flag("dot"),
    caseInsensitive: flag("caseInsensitive"),
    __engine: Object.hasOwn(input, "__engine") ? input.__engine : undefined,
  };
}

function buildEngine(asts, opts) {
  const ci = !!opts.caseInsensitive;
  const dot = !!opts.dot;
  // Members that only differ in spelling factor into one, which may be
  // a literal again.
  const body = asts.length === 1 ? asts[0].body : factorBranches(asts.map((a) => a.body));
  const literalBytes = nodeToLiteralBytes(body);
  if (literalBytes !== null) return new LiteralMatcher(literalBytes, ci);
  const program = lower(body, ci);
  return SegmentMatcher.build(program, dot, {
    forceNfa: opts.__engine === "nfa",
    byteOnly: opts.__engine === "bytes",
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

  const matchDir = (input) => {
    requireStringInput(input);
    if (positiveEngine === null) return DirMatch.Descend;
    const dm = positiveEngine.matchDir(input);
    if (hasNegatives) {
      return DirMatch.from(DirMatch.matches(dm), true, DirMatch.matchesAllBelow(dm));
    }
    return dm;
  };

  const staticPrefixes = () =>
    hasNegatives ? [new Uint8Array(0)] : positiveEngine.staticPrefixes();

  return { match, matchDir, staticPrefixes };
}
