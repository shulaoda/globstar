// Multi-pattern matcher comparison: globstar's union vs picomatch /
// minimatch / micromatch, each matching a set the way it offers to.
// Output is consumed by `scripts/bench.mjs`.

import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { globstar } from "@globstar/core";
import { PATTERN_SETS, fresh, matcherBench } from "./shared.js";

// picomatch and micromatch take a list of patterns themselves (their
// matcher tries the members in order and stops at the first hit).
// minimatch has no list form, so its column loops one matcher per
// pattern the same way, as a caller would.
function buildPerPattern(compileSingle) {
  return (patterns) => patterns.map(compileSingle);
}
function runPerPattern(matchOne) {
  return (arr, s) => {
    for (let i = 0; i < arr.length; i++) {
      if (matchOne(arr[i], s)) return true;
    }
    return false;
  };
}

// Every library answers with dot files matched, as globstar does by
// default and as the Rust bench sets for all.
const DOT = { dot: true };

await matcherBench(
  "Pattern set",
  PATTERN_SETS,
  [
    ["globstar", (patterns) => globstar(patterns), (fn, s) => fn(s)],
    ["picomatch", (patterns) => pico(patterns, DOT), (m, s) => m(s)],
    [
      "minimatch",
      buildPerPattern((b) => new Minimatch(b, DOT)),
      runPerPattern((m, s) => m.match(s)),
    ],
    ["micromatch", (patterns) => micro.matcher(patterns, DOT), (m, s) => m(s)],
  ],
  (patterns) => patterns.map(fresh),
);
