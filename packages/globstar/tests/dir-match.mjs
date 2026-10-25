import assert from "node:assert/strict";
import { compileMatcher, DirMatch } from "../src/index.js";

for (const [d, matches] of [
  ["lib", true],
  ["src", true],
  ["lib/a.test.ts", false],
]) {
  const m = compileMatcher(["src/**", "!**/*.test.ts"]);
  assert.equal(DirMatch.matches(m.matchDir(d)), matches, d);
  assert.equal(DirMatch.matches(m.matchDir(d)), m.match(d), d);
}

for (const v of [0, 1, 2, 3, 4, 5]) {
  assert.equal(
    DirMatch.from(DirMatch.matches(v), DirMatch.mayMatchBelow(v), DirMatch.matchesAllBelow(v)),
    v,
  );
  assert.ok(!DirMatch.matchesAllBelow(v) || DirMatch.mayMatchBelow(v));
}
assert.equal(DirMatch.from(false, false, true), DirMatch.DescendAll);

{
  const m = compileMatcher(["src/**", "!b"]);
  assert.ok(m.match("lib/x.ts"));
  assert.ok(DirMatch.mayMatchBelow(m.matchDir("lib")));
  assert.ok(!DirMatch.matchesAllBelow(m.matchDir("lib")));
}

console.log("✓ matchDir with a negated member, and the DirMatch helpers");

if (process.platform === "win32") {
  const text = (m) => m.staticPrefixes().map((b) => new TextDecoder().decode(b));
  assert.deepEqual(text(compileMatcher(["src\\\\sub/*.ts", "src/sub/*.ts"])), ["src/sub"]);
  assert.deepEqual(text(compileMatcher("src\\\\sub/*.ts")), []);
  assert.deepEqual(text(compileMatcher("a\\\\b")), []);
  assert.equal(compileMatcher("a/b\\\\c/*.ts").matchDir("a"), DirMatch.Pruned);
  console.log("✓ an escaped backslash is dead on Windows");
}
