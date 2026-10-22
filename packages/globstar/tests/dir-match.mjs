// `matchDir` where the shared corpus cannot reach: a list with a negated
// member (Rust's `Glob::union` refuses one) and the `DirMatch` helpers.
// The verdicts of plain patterns and lists are rows of
// `fixtures/corpus/corpus-dir.txt` and `corpus-multi-dir.txt`.

import assert from "node:assert/strict";
import { compileMatcher, DirMatch } from "../src/index.js";

// A negated member says exactly whether the directory itself matches
// (§15.4).
for (const [d, matches] of [
  ["lib", true],
  ["src", true],
  ["lib/a.test.ts", false],
]) {
  const m = compileMatcher(["src/**", "!**/*.test.ts"]);
  assert.equal(DirMatch.matches(m.matchDir(d)), matches, d);
  assert.equal(DirMatch.matches(m.matchDir(d)), m.match(d), d);
}

// The three facts name the verdict, and all-below implies may-below.
for (const v of [0, 1, 2, 3, 4, 5]) {
  assert.equal(
    DirMatch.from(DirMatch.matches(v), DirMatch.mayMatchBelow(v), DirMatch.matchesAllBelow(v)),
    v,
  );
  assert.ok(!DirMatch.matchesAllBelow(v) || DirMatch.mayMatchBelow(v));
}
assert.equal(DirMatch.from(false, false, true), DirMatch.DescendAll);

// A union with a negated member may match anywhere (what `!b` keeps is
// everything but `b`): a walker must never prune a folder because of it.
{
  const m = compileMatcher(["src/**", "!b"]);
  assert.ok(m.match("lib/x.ts"));
  assert.ok(DirMatch.mayMatchBelow(m.matchDir("lib")));
  assert.ok(!DirMatch.matchesAllBelow(m.matchDir("lib")));
}

console.log("✓ matchDir with a negated member, and the DirMatch helpers");

// Windows: an escaped `\` is a literal no segment holds (§4.2), so its
// expansion seeds nothing, and `matchDir` prunes where it is next. Twin
// of `an_escaped_backslash_is_dead_on_windows`.
if (process.platform === "win32") {
  const text = (m) => m.staticPrefixes().map((b) => new TextDecoder().decode(b));
  assert.deepEqual(text(compileMatcher(["src\\\\sub/*.ts", "src/sub/*.ts"])), ["src/sub"]);
  assert.deepEqual(text(compileMatcher("src\\\\sub/*.ts")), []);
  assert.deepEqual(text(compileMatcher("a\\\\b")), []);
  assert.equal(compileMatcher("a/b\\\\c/*.ts").matchDir("a"), DirMatch.Pruned);
  console.log("✓ an escaped backslash is dead on Windows");
}
