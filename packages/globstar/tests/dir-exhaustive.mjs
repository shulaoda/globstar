// Every pattern of a fixed segment alphabet (14 tokens at 1–2 segments, 8
// at 3) against every path of a fixed universe, for the properties that
// define matchDir relative to match: the match flag is match(dir); a
// directory with a matching path strictly below it descends; Pruned means
// no descendant matches; the anchored chains and the NFA agree on every
// match; the all-below flag is set exactly when every path up to three
// levels below matches (deeper than any pattern, so such a path matches
// only through a `**`).

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
// `c` matches nothing literal in the alphabet.
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
// Every path of 1..=3 PSEG segments, shallowest first.
const universe = [...enumerate(PSEG, 1), ...enumerate(PSEG, 2), ...enumerate(PSEG, 3)];

const allBelow = (matcher, dir, set = universe) => set.every((s) => matcher.match(`${dir}/${s}`));

// Every match starts with one of the static prefixes, byte for byte.
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

// `pats` are patterns, or arrays of patterns for union matchers; `set` is
// the suffixes all-below is judged on.
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
    // Under caseInsensitive a match may differ in case from its prefix.
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

// Every ordered pair of eight patterns, every ordered triple of four.
const UNION_PATTERNS = ["a/b", "a/*", "*/b", "**/b", "a/**", "{a,b}/c", ".a/*", "?"];
const UNION_CORE = ["a/*", "**/b", ".a/*", "?"];
const unionSets = [];
for (const a of UNION_PATTERNS) for (const b of UNION_PATTERNS) unionSets.push([a, b]);
for (const a of UNION_CORE)
  for (const b of UNION_CORE) for (const c of UNION_CORE) unionSets.push([a, b, c]);
total += check(unionSets, universe, false, false);
total += check(unionSets, universe, true, false);

// A globstar, then three elements: the chain's tail tokens take one or two
// of them. Paths go four deep, all-below three.
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
