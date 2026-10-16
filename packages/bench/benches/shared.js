// Helpers the matcher benches share.

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
