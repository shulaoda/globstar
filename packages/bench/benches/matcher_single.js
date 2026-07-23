// Single-pattern matcher comparison: globstar (default API) vs
// picomatch / minimatch / micromatch. Three phases (compile / match /
// memory). Output is consumed by `bench.mjs` to populate
// BENCHMARKS.md.

import { bench, run, group, summary, do_not_optimize } from "mitata";
import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { compileMatcher } from "../../globstar/src/glob.js";

if (!global.gc) {
  console.error("Run with --expose-gc");
  process.exit(1);
}

const PATTERNS = [
  ["src/main.rs", "literal"],
  ["src/*.ts", "simple-wildcard"],
  ["src/**/*.ts", "globstar"],
  ["**/*.{ts,tsx,js,jsx}", "brace-suffix"],
  ["**/*.md", "reject-prefilter"],
  ["src/**/n*d[k-m]e?txt", "class-anychar"],
  ["src/**/{tob,crazy}/?*.{png,txt}", "brace-anychar"],
];
const PATHS = [
  "src/main.ts",
  "src/cli/commands/build.ts",
  "src/cli/commands/run.rs",
  "src/main.rs",
  "tests/smoke.ts",
  "dist/bundle.js",
  "node_modules/foo/index.js",
  "package.json",
  ".env",
  "target/debug/build/foo",
  "src/a/bigger/path/to/the/crazy/needle.txt",
];
const N = PATHS.length;

const LIBS = [
  ["globstar", (p) => compileMatcher(p).match, (m, s) => m(s)],
  ["picomatch", (p) => pico(p), (m, s) => m(s)],
  ["minimatch", (p) => new Minimatch(p), (m, s) => m.match(s)],
  ["micromatch", (p) => micro.matcher(p), (m, s) => m(s)],
];

console.log("===== PHASE 1: COMPILE =====\n");
for (const [p, label] of PATTERNS) {
  group(`compile: ${label}`, () => {
    summary(() => {
      for (const [name, build] of LIBS) {
        bench(name, () => do_not_optimize(build(p)));
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
function trialMem(build, p) {
  for (let i = 0; i < 100; i++) build(p);
  gcSweep();
  const before = snapHeap();
  const arr = Array.from({ length: N_TRIAL });
  for (let i = 0; i < N_TRIAL; i++) arr[i] = build(p);
  const after = snapHeap();
  if (arr[0] === undefined) console.log("!");
  return Math.max(0, (after - before) / N_TRIAL);
}
function median(xs) {
  return xs.slice().sort((a, b) => a - b)[Math.floor(xs.length / 2)];
}

// Header chosen to match the parser hint in `bench.mjs`.
console.log(`\n===== PHASE 3: MEMORY (median of ${TRIALS}, B/matcher) =====\n`);
const widths = [22, 12, 12, 12, 12];
const header = ["Pattern", ...LIBS.map(([n]) => n)]
  .map((s, i) => s.padEnd(widths[i] || 12))
  .join(" | ");
console.log(header);
console.log("-".repeat(header.length));
for (const [p, label] of PATTERNS) {
  const cells = [label.padEnd(widths[0])];
  let i = 1;
  for (const [, build] of LIBS) {
    const xs = [];
    for (let t = 0; t < TRIALS; t++) xs.push(trialMem(build, p));
    cells.push(`${Math.round(median(xs))} B`.padStart(widths[i] || 12));
    i++;
  }
  console.log(cells.join(" | "));
}
