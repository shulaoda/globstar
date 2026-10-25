import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { globstar } from "@globstar/core";
import { PATTERN_SETS, fresh, matcherBench } from "./shared.js";

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
