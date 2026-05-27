// Matcher factory.
//
// Public:    globstar(patterns, options?)       → (input) => boolean
// Internal:  compileMatcher(patterns, options?) → { match, matchDir, staticPrefixes }
//
// `compileMatcher` is exported here but never re-exported from
// `src/index.js`, so `matchDir` / `staticPrefixes` stay
// package-private. Walker imports `compileMatcher` directly.
//
// Multi-pattern combines via OR. Each pattern's own `!`-prefix
// applies independently; auto-splitting `!` into ignores is the
// walker's job, not the matcher's. By the time walker calls us, all
// patterns it passes are strictly positive.
//
// Engine pick (in order):
//   pure literal pattern   → LiteralMatcher (separator-aware byte compare)
//   `__engine: "dfa"`      → ThompsonDfa, PikeVm fallback if state cap blown
//   default / "pikevm"     → PikeVm (bitmask NFA simulator)
//
// Why default = PikeVm: most direct callers shape like
// `globstar(p)(input)` — one or two matches against a freshly compiled
// matcher. PikeVm compile is 3–9× cheaper than DFA build (no subset
// construction); its match is 2–6× slower per byte, but for single-
// shot use the total cost is dominated by compile and PikeVm wins.
// Walker amortizes thousands of matches per matcher and explicitly
// opts into DFA via `__engine: "dfa"` (set by `toMatcherOptions`).

import { parse } from "./parser.js";
import { lower, dedupePrefixes } from "./engine/ops.js";
import { compileNfaSoa } from "./engine/nfa-soa.js";
import { LiteralMatcher } from "./engine/literal.js";
import { ThompsonDfa } from "./engine/thompson-dfa.js";
import { PikeVm } from "./engine/pikevm.js";
import { nodeToLiteralBytes } from "./ast.js";
import { factorBranches } from "./factor.js";
import { GlobError } from "./error.js";
import { DirMatch } from "./dir-match.js";
import { toBytes } from "./utf8.js";

const DEFAULT_OPTIONS = { dot: true, caseInsensitive: false };

// NFA state count above which a multi-pattern union decomposes into
// per-pattern engines wrapped in `OrEngine` instead of building one
// merged DFA. Matches the DFA's StateKey fast-path budget — beyond
// 64 states subset construction enters the wide path whose compile
// cost balloons (huge-set: 2.3 ms at 223 NFA states vs ~100 µs
// decomposed). 95% of realistic patterns stay under this threshold.
const NFA_FAST_PATH_LIMIT = 64;

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

  // Positive branches collapse into one engine via `factorBranches`
  // (shared prefix/suffix → smaller NFA / DFA).
  const positiveEngine = positiveBodies.length > 0 ? buildEngine(positiveBodies, opts) : null;
  // Negative branches stay as N independent engines, each contributing
  // `!body.match(input)` to the OR. Rare path; not worth factoring.
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

  // Probe merged NFA size for both engines. Above the fast-path
  // budget, decompose into per-pattern children:
  //
  // - DFA: wide-path subset construction's compile cost balloons at
  //   N > 64 (huge-set: 2.3 ms). Decomposition keeps each child on
  //   the u64-keyed fast path.
  // - PikeVM: compile is fine at any N, but match-time active-set
  //   scan is O(N_states/byte). Decomposition lets each child's
  //   facts.suffix prefilter eliminate most candidates upfront, so
  //   huge-set match drops 1650 → 640 ns (~2.6× faster).
  //
  // The Thompson probe is cheap (~5-15 µs even at 223 states) and
  // only fires for unions, so single-pattern callers don't pay it.
  // `__noAutoOr` bypasses this — used by benches that need to
  // measure the merged-engine path explicitly.
  if (bodies.length > 1 && !opts.__noAutoOr) {
    const nfa = compileNfaSoa(program, dot);
    if (nfa.n > NFA_FAST_PATH_LIMIT) {
      const children = bodies.map((body) => buildEngine([body], opts));
      return new OrEngine(children);
    }
  }

  if (opts.__engine === "dfa") {
    return ThompsonDfa.build(program, dot) ?? PikeVm.build(program, dot);
  }
  return PikeVm.build(program, dot);
}

// Per-pattern decomposition wrapper. Same `isMatch` / `matchDir` /
// `staticPrefixes` interface as the concrete engines so the surrounding
// `compileMatcher` plumbing stays untouched.
class OrEngine {
  constructor(children) {
    this.children = children;
    // Static prefixes cached at construction — walker calls
    // `staticPrefixes()` once per traversal seed and we don't want
    // the per-call dedupe.
    const all = [];
    for (const c of children) for (const p of c.staticPrefixes()) all.push(p);
    this.cachedPrefixes = dedupePrefixes(all);
  }

  isMatch(input) {
    for (let i = 0; i < this.children.length; i++) {
      if (this.children[i].isMatch(input)) return true;
    }
    return false;
  }

  matchDir(input) {
    let exact = false;
    let prefix = false;
    for (let i = 0; i < this.children.length; i++) {
      const dm = this.children[i].matchDir(input);
      if (dm === DirMatch.DescendAndMatch) return DirMatch.DescendAndMatch;
      if (dm === DirMatch.Match) exact = true;
      else if (dm === DirMatch.Descend) prefix = true;
      if (exact && prefix) return DirMatch.DescendAndMatch;
    }
    return DirMatch.fromExactPrefix(exact, prefix);
  }

  staticPrefixes() {
    return this.cachedPrefixes;
  }
}

function makeMatcher(positiveEngine, negativeEngines) {
  const hasNegatives = negativeEngines.length > 0;

  // Closure-bound (not method-shorthand) so `compileMatcher(...).match`
  // can be passed as a free function reference without `this` concerns.
  // Engines work on UTF-8 bytes (matching the parser's encoding contract);
  // strings get encoded once here so callers can keep passing JS strings.
  const match = (input) => {
    const bytes = toBytes(input);
    if (positiveEngine !== null && positiveEngine.isMatch(bytes)) return true;
    for (let i = 0; i < negativeEngines.length; i++) {
      if (!negativeEngines[i].isMatch(bytes)) return true; // `!body.match(p) === true`
    }
    return false;
  };

  const matchDir = (input) => {
    if (positiveEngine === null) return DirMatch.Descend;
    const bytes = toBytes(input);
    const dm = positiveEngine.matchDir(bytes);
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
