// Helpers the matcher benches share.

import { readFileSync } from "node:fs";

// The data lines of a fixture: not blank, not a `#` comment.
const fixture = (name) =>
  readFileSync(new URL(`../../../fixtures/${name}`, import.meta.url), "utf8")
    .split("\n")
    .filter((l) => l !== "" && !l.startsWith("#"));

// The bench corpus, from the same files the Rust benches read, so the
// rows of the two runtimes line up by construction.
export const PATHS = fixture("bench-paths.txt");
export const PATTERNS = fixture("bench-single.tsv").map((l) => {
  const [label, pattern] = l.split("\t");
  return [pattern, label];
});
/** @type {Array<[string, string[]]>} */
export const PATTERN_SETS = [
  ...fixture("bench-sets.tsv").map((l) => {
    const [label, ...patterns] = l.split("\t");
    return [label, patterns];
  }),
  ["ignore-list-40", fixture("bench-ignore-list.txt")],
  ["file-list-100", fixture("bench-file-list.txt")],
];

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

// A path no row matches. A first match on it runs every part a matcher
// left for its first use: in a list matched one member at a time, each
// member, where a hit would stop at the first.
export const MISS = "target/debug/build/foo";

// Throws unless `path` is a miss for every input.
export function assertMisses(libs, inputs, path) {
  const [[name, build, run]] = libs;
  for (const input of inputs) {
    if (run(build(input), path)) {
      throw new Error(`${name} matches ${path} with ${JSON.stringify(input)}: not a miss`);
    }
  }
}

// A fresh variant of every member of a pattern set.
export function freshSet(patterns) {
  return patterns.map(fresh);
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
