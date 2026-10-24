// globstar's union against picomatch, minimatch and micromatch, each
// matching a set the way it offers to; `scripts/bench.mjs` reads the
// output.

import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { globstar } from "@globstar/core";
import { PATTERN_SETS, fresh, matcherBench } from "./shared.js";

// picomatch and micromatch take a list themselves (the members in order,
// stopping at the first hit); minimatch has no list form, so its column
// loops one matcher per pattern the same way.
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

// As globstar's default, and as the Rust bench sets for all.
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
