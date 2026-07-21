// Multi-pattern matcher comparison: globstar (default API) vs
// picomatch / minimatch / micromatch (per-pattern matchers OR'd at
// runtime). Output is consumed by `bench.mjs`.

import { bench, run, group, summary, do_not_optimize } from "mitata";
import pico from "picomatch";
import { Minimatch } from "minimatch";
import micro from "micromatch";
import { compileMatcher } from "../../globstar/src/matcher/glob.js";
import { compileMatcher as compileSegment } from "../../globstar-segment/src/index.js";

if (!global.gc) {
  console.error("Run with --expose-gc");
  process.exit(1);
}

/** @type {Array<[string, string[]]>} */
const PATTERN_SETS = [
  ["solo-globstar", ["**/*.ts"]],
  ["brace-equiv-4", ["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"]],
  [
    "brace-equiv-8",
    ["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx", "**/*.mjs", "**/*.cjs", "**/*.mts", "**/*.cts"],
  ],
  ["mixed-roots", ["src/**/*.ts", "tests/**/*.ts", "lib/**/*.js"]],
  // Aligned with Rust's matcher_multi.rs corpus — `huge-set-pos` is the
  // 10-positive variant (Rust's `Glob::union` rejects `!`-prefixed
  // patterns so the negation-bearing `huge-set` and `include-exclude`
  // sets only ran on the JS side and were dropped to keep
  // BENCHMARKS.md cross-runtime tables consistent).
  [
    "huge-set-pos",
    [
      "src/**/*.ts",
      "src/**/*.tsx",
      "tests/**/*.test.ts",
      "lib/**/*.{js,mjs}",
      "**/*.json",
      "**/package.json",
      "**/.env*",
      "components/**/*.vue",
      "scripts/**/*.sh",
      "docs/**/*.md",
    ],
  ],
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

// Per-pattern + Array.some for the third-party libs (no native union API).
function buildPerPattern(compileSingle) {
  return (patterns) =>
    patterns.map((p) => {
      const neg = p.startsWith("!");
      const body = neg ? p.slice(1) : p;
      return { neg, m: compileSingle(body) };
    });
}
function runPerPattern(matchOne) {
  return (arr, s) => {
    let any = false;
    for (let i = 0; i < arr.length; i++) {
      const { neg, m } = arr[i];
      const ok = matchOne(m, s);
      if (neg ? !ok : ok) any = true;
    }
    return any;
  };
}

const LIBS = [
  // globstar handles `!`-prefixed patterns natively in `compileMatcher`.
  ["globstar", (patterns) => compileMatcher(patterns).match, (fn, s) => fn(s)],
  ["globstar-ssm", (patterns) => compileSegment(patterns).match, (fn, s) => fn(s)],
  ["picomatch", buildPerPattern((b) => pico(b)), runPerPattern((m, s) => m(s))],
  ["minimatch", buildPerPattern((b) => new Minimatch(b)), runPerPattern((m, s) => m.match(s))],
  ["micromatch", buildPerPattern((b) => micro.matcher(b)), runPerPattern((m, s) => m(s))],
];

console.log("===== PHASE 1: COMPILE =====\n");
for (const [label, patterns] of PATTERN_SETS) {
  group(`compile: ${label}`, () => {
    summary(() => {
      for (const [name, build] of LIBS) {
        bench(name, () => do_not_optimize(build(patterns)));
      }
    });
  });
}

console.log("\n===== PHASE 2: MATCH (precompiled) =====\n");
for (const [label, patterns] of PATTERN_SETS) {
  const matchers = LIBS.map(([name, build, runFn]) => [name, build(patterns), runFn]);
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

// ── Phase 3: memory ───────────────────────────────────────────────
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

console.log(`\n===== PHASE 3: MEMORY (median of ${TRIALS}, B/matcher) =====\n`);
const widths = [22, 12, 12, 12, 12];
const header = ["Pattern set", ...LIBS.map(([n]) => n)]
  .map((s, i) => s.padEnd(widths[i] || 12))
  .join(" | ");
console.log(header);
console.log("-".repeat(header.length));
for (const [label, patterns] of PATTERN_SETS) {
  const cells = [label.padEnd(widths[0])];
  let i = 1;
  for (const [, build] of LIBS) {
    const xs = [];
    for (let t = 0; t < TRIALS; t++) xs.push(trialMem(build, patterns));
    cells.push(`${Math.round(median(xs))} B`.padStart(widths[i] || 12));
    i++;
  }
  console.log(cells.join(" | "));
}
