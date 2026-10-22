// What the two matcher benches share: the bench corpus, the answer
// checks, and the bench itself, which `matcher_single.js` and
// `matcher_multi.js` call with their libraries.

import { readFileSync } from "node:fs";
import { bench, run, group, summary, do_not_optimize } from "mitata";

// The data lines of a fixture: not blank, not a `#` comment.
const fixture = (name) =>
  readFileSync(new URL(`../../../fixtures/${name}`, import.meta.url), "utf8")
    .split("\n")
    .filter((l) => l !== "" && !l.startsWith("#"));

// The bench corpus, from the same files the Rust benches read, so the
// rows of the two runtimes line up by construction.
export const PATHS = fixture("bench-paths.txt");
/** @type {Array<[string, string]>} */
export const PATTERNS = fixture("bench-single.tsv").map((l) => {
  const [label, pattern] = l.split("\t");
  return [label, pattern];
});
/** @type {Array<[string, string[]]>} */
export const PATTERN_SETS = [
  ...fixture("bench-sets.tsv").map((l) => {
    const [label, ...patterns] = l.split("\t");
    return [label, patterns];
  }),
  ["ignore-list-40", fixture("bench-ignore-list.txt")],
  ["file-list-100", fixture("bench-file-list.txt")],
  ["gitignore-89", fixture("bench-gitignore-list.txt")],
];
// Every pattern of the bench but the three real .gitignore sets in one
// union, each once.
const ALL_CASES = [
  ...new Set([...PATTERNS.map(([, p]) => p), ...PATTERN_SETS.flatMap(([, ps]) => ps)]),
];
PATTERN_SETS.push(
  ["global-586", fixture("bench-gitignore-global.txt")],
  ["openssl-500", fixture("bench-gitignore-openssl.txt")],
  ["joomla-705", fixture("bench-gitignore-joomla.txt")],
  ["all-cases", ALL_CASES],
);

let count = 0;

// A pattern this process has not compiled yet, of the same shape as a
// bench row: a short literal tag on the end, or inside the last branch
// when it ends with a brace. V8 caches a RegExp by its source and
// compiles it on first use, so rebuilding one string and never matching
// made the regex libraries look nearly free; the compile and memory
// phases build from these and use each matcher before they count it.
export function fresh(pattern) {
  const tag = "q" + (count++).toString(36);
  return pattern.endsWith("}") ? pattern.slice(0, -1) + tag + "}" : pattern + tag;
}

// A path no row matches. A first call on it runs every part a matcher
// left for its first use: in a list matched one member at a time, each
// member, where a hit would stop at the first.
export const MISS = "crates/core/src/foo";

// Throws unless `path` is a miss for every input.
export function assertMisses(libs, inputs, path) {
  const [[name, build, run]] = libs;
  for (const input of inputs) {
    if (run(build(input), path)) {
      throw new Error(`${name} matches ${path} with ${JSON.stringify(input)}: not a miss`);
    }
  }
}

// Throws unless every library gives the first one's answer on every
// (input, path): the rows must time the same answers.
export function assertAgree(libs, inputs, paths) {
  for (const input of inputs) {
    const [[first, build0, run0], ...rest] = libs;
    const want = build0(input);
    const others = rest.map(([name, build, run]) => [name, build(input), run]);
    for (const path of paths) {
      const expected = run0(want, path);
      for (const [name, m, run] of others) {
        if (run(m, path) !== expected) {
          throw new Error(`${name} and ${first} disagree on ${JSON.stringify(input)} / ${path}`);
        }
      }
    }
  }
}

// The bench: every library's answers checked against the first's, then
// mitata groups per row for the build, the build and first non-match,
// and the match of a prebuilt matcher, then the memory table `bench.mjs`
// parses (`header` is its hint). `rows` are `[label, input]`;
// `freshen(input)` gives an input not built before, of the same shape.
export async function matcherBench(header, rows, libs, freshen) {
  if (!global.gc) {
    console.error("Run with --expose-gc");
    process.exit(1);
  }
  const inputs = rows.map(([, input]) => input);
  assertAgree(libs, inputs, PATHS);
  assertMisses(libs, inputs, MISS);

  // A matcher for an input not seen before: the build alone, then the
  // build and its first call, a non-match, which is what a new pattern
  // costs before it answers (a library may leave work for the first
  // call, such as compiling its RegExp). The miss makes a library that
  // matches one member at a time run every member.
  console.log("===== PHASE 1: COMPILE =====\n");
  for (const [label, input] of rows) {
    group(`build: ${label}`, () => {
      summary(() => {
        for (const [name, build] of libs) {
          bench(name, () => do_not_optimize(build(freshen(input))));
        }
      });
    });
    group(`compile: ${label}`, () => {
      summary(() => {
        for (const [name, build, call] of libs) {
          bench(name, () => do_not_optimize(call(build(freshen(input)), MISS)));
        }
      });
    });
  }

  console.log("\n===== PHASE 2: MATCH (precompiled) =====\n");
  for (const [label, input] of rows) {
    const matchers = libs.map(([name, build, call]) => [name, build(input), call]);
    group(`match: ${label}`, () => {
      summary(() => {
        for (const [name, m, call] of matchers) {
          bench(name, function* () {
            let i = 0;
            yield () => do_not_optimize(call(m, PATHS[i++ % PATHS.length]));
          });
        }
      });
    });
  }
  await run({ format: "mitata" });

  console.log(`\n===== PHASE 3: MEMORY (median of ${TRIALS}, B/matcher) =====\n`);
  const widths = [22, 12, 16, 12, 12, 12];
  const head = [header, ...libs.map(([n]) => n)]
    .map((s, i) => s.padEnd(widths[i] || 12))
    .join(" | ");
  console.log(head);
  console.log("-".repeat(head.length));
  for (const [label, input] of rows) {
    const cells = [label.padEnd(widths[0])];
    libs.forEach(([, build, call], i) => {
      const xs = [];
      for (let t = 0; t < TRIALS; t++) xs.push(trialMem(build, call, input, freshen));
      cells.push(`${Math.round(median(xs))} B`.padStart(widths[i + 1] || 12));
    });
    console.log(cells.join(" | "));
  }
}

// Matchers built per trial, and the members they may hold between
// them: the regex libraries compile a RegExp per member, and V8's code
// space holds about that many, so a wide union is measured from fewer
// matchers (each big enough to measure alone).
const N_TRIAL = 1000;
const MEMBERS_PER_TRIAL = 50_000;
const TRIALS = 5;

function gcSweep() {
  for (let i = 0; i < 3; i++) global.gc();
}
function snapHeap() {
  gcSweep();
  const m = process.memoryUsage();
  return m.heapUsed + m.arrayBuffers;
}
// What N_TRIAL matchers for distinct inputs keep once each has matched
// every bench path.
function trialMem(build, call, input, freshen) {
  const members = Array.isArray(input) ? input.length : 1;
  const n = Math.min(N_TRIAL, Math.max(20, Math.floor(MEMBERS_PER_TRIAL / members)));
  for (let i = 0; i < 100; i++) call(build(freshen(input)), PATHS[0]);
  gcSweep();
  const before = snapHeap();
  const arr = Array.from({ length: n });
  for (let i = 0; i < n; i++) {
    arr[i] = build(freshen(input));
    for (const path of PATHS) call(arr[i], path);
  }
  const after = snapHeap();
  if (arr[0] === undefined) console.log("!");
  return Math.max(0, (after - before) / n);
}
function median(xs) {
  return xs.slice().sort((a, b) => a - b)[Math.floor(xs.length / 2)];
}
