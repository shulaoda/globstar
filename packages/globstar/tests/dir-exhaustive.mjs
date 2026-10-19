// Bounded-exhaustive matchDir verification — JS twin of
// crates/globstar/tests/dir_exhaustive.rs (identical enumeration).
//
// Every pattern from a fixed segment alphabet (full 14-token set at 1-2
// segments, core 8-token set at 3) runs against every path in a fixed
// universe, checking the properties that define matchDir relative to
// match:
//   1. the match flag is exactly match(dir);
//   2. any matching universe path strictly below the dir forces descent
//      (a pruning walker must never lose a match);
//   3. Pruned implies no universe descendant matches;
//   4. the segment engine's anchored chains and its NFA agree on every
//      match (`__engine: "nfa"`);
//   5. the all-below flag is set exactly when every path up to three
//      levels below the dir matches — a directory is skipped whole only
//      when nothing below it escapes. Three levels is past the deepest
//      pattern, so a path that deep matches only through a `**`.
//
// Deterministic: no randomness — the whole bounded space is covered.
//
// Run: node packages/globstar/tests/dir-exhaustive.mjs

import assert from "node:assert/strict";
import { compileMatcher } from "../src/glob.js";
import { DirMatch } from "../src/dir-match.js";

const SEGMENTS = [
  "a",
  "b",
  "ab",
  "*",
  "?",
  "a*",
  "*a",
  "[ab]",
  "[!a]",
  ".a",
  "{a,b}",
  "{a,*}",
  "**",
  ".*",
];
const CORE = ["a", "b", "*", "?", "[ab]", ".a", "{a,b}", "**"];
const PSEG = ["a", "b", "ab", "c", ".a"];

function enumerate(segs, depth) {
  const out = [];
  const idx = Array.from({ length: depth }, () => 0);
  for (;;) {
    out.push(idx.map((i) => segs[i]).join("/"));
    let d = depth;
    for (;;) {
      if (d === 0) return out;
      d--;
      idx[d]++;
      if (idx[d] < segs.length) break;
      idx[d] = 0;
    }
  }
}

const patterns = [...enumerate(SEGMENTS, 1), ...enumerate(SEGMENTS, 2), ...enumerate(CORE, 3)];
// Every relative path of 1..=3 PSEG segments, shallowest first: the
// directories judged, and the suffixes all-below is judged on.
const universe = [...enumerate(PSEG, 1), ...enumerate(PSEG, 2), ...enumerate(PSEG, 3)];

// Does every `dir + "/" + suffix` match?
const allBelow = (matcher, dir, set = universe) => set.every((s) => matcher.match(`${dir}/${s}`));

// Every match starts with one of the static prefixes, byte for byte.
// (Under caseInsensitive a match may differ in case: not checked.)
function assertPrefixesCover(matcher, paths, matched) {
  const prefixes = matcher.staticPrefixes().map((b) => new TextDecoder().decode(b));
  paths.forEach((path, i) => {
    if (!matched[i]) return;
    const ok = prefixes.some((q) => q === "" || path === q || path.startsWith(q + "/"));
    assert.ok(ok, `${JSON.stringify(path)} starts with none of ${JSON.stringify(prefixes)}`);
  });
}

const isBelow = (child, dir) =>
  child.length > dir.length && child[dir.length] === "/" && child.startsWith(dir);

// `pats` are patterns, or arrays of patterns for union matchers; `set`
// is the suffixes all-below is judged on.
function check(pats, paths, dot, ci, set = universe) {
  const below = paths.map((d) => {
    const out = [];
    for (let j = 0; j < paths.length; j++) if (isBelow(paths[j], d)) out.push(j);
    return out;
  });

  let cases = 0;
  for (const pattern of pats) {
    const opts = { dot, caseInsensitive: ci };
    const def = compileMatcher(pattern, opts);
    const nfa = compileMatcher(pattern, { ...opts, __engine: "nfa" });

    const matched = paths.map((p) => def.match(p));
    if (!ci) assertPrefixesCover(def, paths, matched);

    for (let i = 0; i < paths.length; i++) {
      const dir = paths[i];
      const ctx = `pattern=${JSON.stringify(pattern)} dir=${JSON.stringify(dir)} dot=${dot} ci=${ci}`;

      assert.equal(nfa.match(dir), matched[i], `is_match disagreement: ${ctx}`);

      const dm = def.matchDir(dir);

      assert.equal(DirMatch.matches(dm), matched[i], `match flag != match: ${ctx} dm=${dm}`);

      const anyBelow = below[i].some((j) => matched[j]);
      if (anyBelow) {
        assert.ok(DirMatch.mayMatchBelow(dm), `walker would lose a match: ${ctx} dm=${dm}`);
      }
      if (dm === DirMatch.Pruned) {
        assert.ok(!anyBelow, `pruned but a descendant matches: ${ctx}`);
      }

      assert.equal(
        DirMatch.matchesAllBelow(dm),
        allBelow(def, dir, set),
        `all-below flag != every descendant matches: ${ctx} dm=${dm}`,
      );
      cases++;
    }
  }
  return cases;
}

let total = 0;
total += check(patterns, universe, false, false);
total += check(patterns, universe, true, false);

// Union matchers: every ordered pair from an 8-pattern set plus every
// ordered triple from a 4-pattern core.
const UNION_PATTERNS = ["a/b", "a/*", "*/b", "**/b", "a/**", "{a,b}/c", ".a/*", "?"];
const UNION_CORE = ["a/*", "**/b", ".a/*", "?"];
const unionSets = [];
for (const a of UNION_PATTERNS) for (const b of UNION_PATTERNS) unionSets.push([a, b]);
for (const a of UNION_CORE)
  for (const b of UNION_CORE) for (const c of UNION_CORE) unionSets.push([a, b, c]);
total += check(unionSets, universe, false, false);
total += check(unionSets, universe, true, false);

// A globstar, then three elements: the chain's tail tokens take one or
// two of them, which no shorter pattern reaches. Paths go four deep, and
// all-below is judged three levels down, as deep as the tail. Twin of
// `dir_exhaustive_long_tails`.
{
  const SEG = ["a", "b", ".a"];
  const pats = ["", "a/", "*/"].flatMap((h) =>
    enumerate(["*", "a", "?", ".a"], 3).map((t) => `${h}**/${t}`),
  );
  const paths = [1, 2, 3, 4].flatMap((d) => enumerate(SEG, d));
  const below = [1, 2, 3].flatMap((d) => enumerate(SEG, d));
  total += check(pats, paths, false, false, below);
  total += check(pats, paths, true, false, below);
}

const CI_SEGMENTS = ["A", "b", "A*", "*A", "[A-B]", "{A,b}"];
const CI_PSEG = ["a", "A", "b", "ab"];
const ciPatterns = [...enumerate(CI_SEGMENTS, 1), ...enumerate(CI_SEGMENTS, 2)];
const ciUniverse = [...enumerate(CI_PSEG, 1), ...enumerate(CI_PSEG, 2)];
total += check(ciPatterns, ciUniverse, true, false);
total += check(ciPatterns, ciUniverse, true, true);

console.log(`✓ matchDir properties hold on ${total} exhaustive pattern×dir cases`);
