// globstar against picomatch, minimatch and micromatch on single
// patterns; `scripts/bench.mjs` reads the output.

import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { globstar } from "@globstar/core";
import { PATTERNS, fresh, matcherBench } from "./shared.js";

// As globstar's default, and as the Rust bench sets for all.
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
