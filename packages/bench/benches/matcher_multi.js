// Multi-pattern matcher comparison: globstar (default API) vs
// picomatch / minimatch / micromatch (per-pattern matchers OR'd at
// runtime). Output is consumed by `scripts/bench.mjs`.

import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { globstar } from "@globstar/core";
import { PATTERN_SETS, freshSet, matcherBench } from "./shared.js";

// One matcher per pattern for the third-party libs (no native union API).
function buildPerPattern(compileSingle) {
  return (patterns) => patterns.map(compileSingle);
}
// Stops at the first hit, as a user's loop would (and as the Rust
// baselines' `.any()` does).
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
    ["picomatch", buildPerPattern((b) => pico(b, DOT)), runPerPattern((m, s) => m(s))],
    [
      "minimatch",
      buildPerPattern((b) => new Minimatch(b, DOT)),
      runPerPattern((m, s) => m.match(s)),
    ],
    ["micromatch", buildPerPattern((b) => micro.matcher(b, DOT)), runPerPattern((m, s) => m(s))],
  ],
  freshSet,
);
