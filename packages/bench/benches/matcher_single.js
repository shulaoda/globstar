// Single-pattern matcher comparison: globstar (default API) vs
// picomatch / minimatch / micromatch. Three phases (compile / match /
// memory). Output is consumed by `scripts/bench.mjs` to populate
// BENCHMARKS.md.

import { bench, run, group, summary, do_not_optimize } from "mitata";
import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { globstar } from "@globstar/core";
import { MISS, PATHS, PATTERNS, assertAgree, assertMisses, fresh } from "./shared.js";

if (!global.gc) {
  console.error("Run with --expose-gc");
  process.exit(1);
}

const N = PATHS.length;

// Every library answers with dot files matched, as globstar does by
// default and as the Rust bench sets for all.
const DOT = { dot: true };
const LIBS = [
  ["globstar", (p) => globstar(p), (m, s) => m(s)],
  ["picomatch", (p) => pico(p, DOT), (m, s) => m(s)],
  ["minimatch", (p) => new Minimatch(p, DOT), (m, s) => m.match(s)],
  ["micromatch", (p) => micro.matcher(p, DOT), (m, s) => m(s)],
];

assertAgree(
  LIBS,
  PATTERNS.map(([p]) => p),
  PATHS,
);

assertMisses(
  LIBS,
  PATTERNS.map(([p]) => p),
  MISS,
);

// A matcher for a pattern not seen before: the build alone, then the
// build and its first match, which is what a new pattern costs before
// it answers (a library may leave work for the first call, such as
// compiling its RegExp).
console.log("===== PHASE 1: COMPILE =====\n");
for (const [p, label] of PATTERNS) {
  group(`build: ${label}`, () => {
    summary(() => {
      for (const [name, build] of LIBS) {
        bench(name, () => do_not_optimize(build(fresh(p))));
      }
    });
  });
  group(`compile: ${label}`, () => {
    summary(() => {
      for (const [name, build, runFn] of LIBS) {
        bench(name, () => do_not_optimize(runFn(build(fresh(p)), MISS)));
      }
    });
  });
}

console.log("\n===== PHASE 2: MATCH (precompiled) =====\n");
for (const [p, label] of PATTERNS) {
  const matchers = LIBS.map(([name, build, runFn]) => [name, build(p), runFn]);
  group(`match: ${label}`, () => {
    summary(() => {
      for (const [name, m, runFn] of matchers) {
        bench(name, function* () {
          let i = 0;
          yield () => do_not_optimize(runFn(m, PATHS[i++ % N]));
        });
      }
    });
  });
}
await run({ format: "mitata" });

// ── Phase 3: memory (median of N trials) ──────────────────────────
function gcSweep() {
  for (let i = 0; i < 3; i++) global.gc();
}
function snapHeap() {
  gcSweep();
  const m = process.memoryUsage();
  return m.heapUsed + m.arrayBuffers;
}
const N_TRIAL = 1000;
const TRIALS = 5;
// What N_TRIAL matchers for distinct patterns keep once each has
// matched every bench path.
function trialMem(build, runFn, p) {
  for (let i = 0; i < 100; i++) runFn(build(fresh(p)), PATHS[0]);
  gcSweep();
  const before = snapHeap();
  const arr = Array.from({ length: N_TRIAL });
  for (let i = 0; i < N_TRIAL; i++) {
    arr[i] = build(fresh(p));
    for (const path of PATHS) runFn(arr[i], path);
  }
  const after = snapHeap();
  if (arr[0] === undefined) console.log("!");
  return Math.max(0, (after - before) / N_TRIAL);
}
function median(xs) {
  return xs.slice().sort((a, b) => a - b)[Math.floor(xs.length / 2)];
}

// Header chosen to match the parser hint in `scripts/bench.mjs`.
console.log(`\n===== PHASE 3: MEMORY (median of ${TRIALS}, B/matcher) =====\n`);
const widths = [22, 12, 16, 12, 12, 12];
const header = ["Pattern", ...LIBS.map(([n]) => n)]
  .map((s, i) => s.padEnd(widths[i] || 12))
  .join(" | ");
console.log(header);
console.log("-".repeat(header.length));
for (const [p, label] of PATTERNS) {
  const cells = [label.padEnd(widths[0])];
  let i = 1;
  for (const [, build, runFn] of LIBS) {
    const xs = [];
    for (let t = 0; t < TRIALS; t++) xs.push(trialMem(build, runFn, p));
    cells.push(`${Math.round(median(xs))} B`.padStart(widths[i] || 12));
    i++;
  }
  console.log(cells.join(" | "));
}
