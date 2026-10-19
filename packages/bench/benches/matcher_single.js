// Single-pattern matcher comparison: globstar (default API) vs
// picomatch / minimatch / micromatch. Output is consumed by
// `scripts/bench.mjs` to populate BENCHMARKS.md.

import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { globstar } from "@globstar/core";
import { PATTERNS, fresh, matcherBench } from "./shared.js";

// Every library answers with dot files matched, as globstar does by
// default and as the Rust bench sets for all.
const DOT = { dot: true };

await matcherBench(
  "Pattern",
  PATTERNS,
  [
    ["globstar", (p) => globstar(p), (m, s) => m(s)],
    ["picomatch", (p) => pico(p, DOT), (m, s) => m(s)],
    ["minimatch", (p) => new Minimatch(p, DOT), (m, s) => m.match(s)],
    ["micromatch", (p) => micro.matcher(p, DOT), (m, s) => m(s)],
  ],
  fresh,
);
