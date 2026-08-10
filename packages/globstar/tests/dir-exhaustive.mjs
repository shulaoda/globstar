// Bounded-exhaustive matchDir verification — JS twin of
// crates/globstar/tests/dir_exhaustive.rs (identical enumeration).
//
// Every pattern from a fixed segment alphabet (full 14-token set at 1-2
// segments, core 8-token set at 3) runs against every path in a fixed
// universe, on the default engine stack and a forced PikeVm, checking the
// properties that define matchDir relative to match:
//   1. the match flag is exactly match(dir);
//   2. any matching universe path strictly below the dir forces descent
//      (a pruning walker must never lose a match);
//   3. Pruned implies no universe descendant matches;
//   4. the two engines agree on every match and every matchDir.
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
  const idx = new Array(depth).fill(0);
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
const universe = [...enumerate(PSEG, 1), ...enumerate(PSEG, 2), ...enumerate(PSEG, 3)];

const isBelow = (child, dir) =>
  child.length > dir.length && child[dir.length] === "/" && child.startsWith(dir);

function check(pats, paths, dot, ci) {
  const below = paths.map((d) => {
    const out = [];
    for (let j = 0; j < paths.length; j++) if (isBelow(paths[j], d)) out.push(j);
    return out;
  });

  let cases = 0;
  for (const pattern of pats) {
    const opts = { dot, caseInsensitive: ci };
    const def = compileMatcher(pattern, opts);
    const pike = compileMatcher(pattern, { ...opts, __engine: "pikevm" });

    const matched = paths.map((p) => def.match(p));

    for (let i = 0; i < paths.length; i++) {
      const dir = paths[i];
      const ctx = `pattern=${JSON.stringify(pattern)} dir=${JSON.stringify(dir)} dot=${dot} ci=${ci}`;

      assert.equal(pike.match(dir), matched[i], `is_match disagreement: ${ctx}`);

      const dm = def.matchDir(dir);
      assert.equal(pike.matchDir(dir), dm, `matchDir disagreement: ${ctx}`);

      assert.equal(DirMatch.isMatch(dm), matched[i], `match flag != match: ${ctx} dm=${dm}`);

      const anyBelow = below[i].some((j) => matched[j]);
      if (anyBelow) {
        assert.ok(DirMatch.shouldDescend(dm), `walker would lose a match: ${ctx} dm=${dm}`);
      }
      if (dm === DirMatch.Pruned) {
        assert.ok(!anyBelow, `pruned but a descendant matches: ${ctx}`);
      }
      cases++;
    }
  }
  return cases;
}

// Same four properties over OR-union matchers, on every ordered pair from
// an 8-pattern set plus every ordered triple from a 4-pattern core.
function checkUnion(sets, paths, dot, ci) {
  const below = paths.map((d) => {
    const out = [];
    for (let j = 0; j < paths.length; j++) if (isBelow(paths[j], d)) out.push(j);
    return out;
  });

  let cases = 0;
  for (const set of sets) {
    const opts = { dot, caseInsensitive: ci };
    const def = compileMatcher(set, opts);
    const pike = compileMatcher(set, { ...opts, __engine: "pikevm" });

    const matched = paths.map((p) => def.match(p));

    for (let i = 0; i < paths.length; i++) {
      const dir = paths[i];
      const ctx = `patterns=${JSON.stringify(set)} dir=${JSON.stringify(dir)} dot=${dot} ci=${ci}`;

      assert.equal(pike.match(dir), matched[i], `union is_match disagreement: ${ctx}`);

      const dm = def.matchDir(dir);
      assert.equal(pike.matchDir(dir), dm, `union matchDir disagreement: ${ctx}`);

      assert.equal(DirMatch.isMatch(dm), matched[i], `union match flag != match: ${ctx} dm=${dm}`);

      const anyBelow = below[i].some((j) => matched[j]);
      if (anyBelow) {
        assert.ok(DirMatch.shouldDescend(dm), `union walker would lose a match: ${ctx} dm=${dm}`);
      }
      if (dm === DirMatch.Pruned) {
        assert.ok(!anyBelow, `union pruned but a descendant matches: ${ctx}`);
      }
      cases++;
    }
  }
  return cases;
}

let total = 0;
total += check(patterns, universe, false, false);
total += check(patterns, universe, true, false);

const UNION_PATTERNS = ["a/b", "a/*", "*/b", "**/b", "a/**", "{a,b}/c", ".a/*", "?"];
const UNION_CORE = ["a/*", "**/b", ".a/*", "?"];
const unionSets = [];
for (const a of UNION_PATTERNS) for (const b of UNION_PATTERNS) unionSets.push([a, b]);
for (const a of UNION_CORE)
  for (const b of UNION_CORE) for (const c of UNION_CORE) unionSets.push([a, b, c]);
total += checkUnion(unionSets, universe, false, false);
total += checkUnion(unionSets, universe, true, false);

const CI_SEGMENTS = ["A", "b", "A*", "*A", "[A-B]", "{A,b}"];
const CI_PSEG = ["a", "A", "b", "ab"];
const ciPatterns = [...enumerate(CI_SEGMENTS, 1), ...enumerate(CI_SEGMENTS, 2)];
const ciUniverse = [...enumerate(CI_PSEG, 1), ...enumerate(CI_PSEG, 2)];
total += check(ciPatterns, ciUniverse, true, false);
total += check(ciPatterns, ciUniverse, true, true);

console.log(`✓ matchDir properties hold on ${total} exhaustive pattern×dir cases`);
