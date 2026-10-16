// Large and deep patterns. JS twin of the size tests in
// `crates/globstar/tests/glob.rs`.

import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";

const lib = JSON.stringify(new URL("../src/index.js", import.meta.url).href);

// Runs `body` (with `compileMatcher` in scope) on a worker with a small
// stack and resolves to what it posts back.
function onSmallStack(body) {
  const code = `
    const { parentPort } = require("node:worker_threads");
    import(${lib}).then(({ compileMatcher }) => parentPort.postMessage((() => { ${body} })()));
  `;
  return new Promise((resolve, reject) => {
    const w = new Worker(code, { eval: true, resourceLimits: { stackSizeMb: 0.5 } });
    w.once("message", resolve);
    w.once("error", reject);
  });
}

// The in-segment closures are built without recursion, so a long
// segment, or a long list under dot=false, fits a small stack.
assert.deepEqual(
  await onSmallStack(`
    const seg = compileMatcher("a" + "*{,}".repeat(2000) + "b");
    const names = Array.from({ length: 2000 }, (_, i) => "*x" + i);
    const list = compileMatcher("{" + names.join(",") + "}", { dot: false });
    return [seg.match("axb"), list.match("ax1999"), list.match(".x1999")];
  `),
  [true, true, false],
);

console.log("✓ large patterns compile on a small stack");
