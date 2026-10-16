// `matchDir` in its two roles. JS twin of `crates/globstar/tests/
// dir_match.rs`: a pattern that selects files decides whether a
// directory is worth walking into; a pattern that excludes files decides
// whether a directory can be skipped whole, or needs no more tests below.

import assert from "node:assert/strict";
import { compileMatcher, DirMatch } from "../src/index.js";

const verdict = (pattern, dir) => compileMatcher(pattern, { dot: true }).matchDir(dir);

// Every verdict has a pattern.
assert.equal(verdict("src/**/*.ts", "lib"), DirMatch.Pruned);
assert.equal(verdict("src", "src"), DirMatch.Match);
assert.equal(verdict("src/**/*.ts", "src"), DirMatch.Descend);
assert.equal(verdict("src{,/**/*.ts}", "src"), DirMatch.DescendAndMatch);
assert.equal(verdict("dist/**", "dist"), DirMatch.DescendAll);
assert.equal(verdict("**", "a"), DirMatch.DescendAllAndMatch);

// Selecting files: walk into a directory unless nothing can match below.
{
  const select = compileMatcher(["src/**/*.ts", "docs/**/*.md"]);
  assert.ok(DirMatch.mayMatchBelow(select.matchDir("")));
  assert.ok(DirMatch.mayMatchBelow(select.matchDir("src")));
  assert.ok(DirMatch.mayMatchBelow(select.matchDir("docs/guide")));
  assert.equal(select.matchDir("node_modules"), DirMatch.Pruned);
  assert.ok(!DirMatch.mayMatchBelow(select.matchDir("node_modules")));
  const all = compileMatcher("src/**", { dot: true });
  assert.ok(DirMatch.matchesAllBelow(all.matchDir("src")));
}

// Excluding files: skip a directory only when everything below is excluded.
{
  const exclude = compileMatcher(["**/node_modules/**", "**/*.log"], { dot: true });
  assert.ok(DirMatch.matchesAllBelow(exclude.matchDir("node_modules")));
  assert.ok(DirMatch.matchesAllBelow(exclude.matchDir("a/b/node_modules")));
  const src = exclude.matchDir("src");
  assert.ok(DirMatch.mayMatchBelow(src) && !DirMatch.matchesAllBelow(src));
  // A file pattern matching the directory's own path says nothing about
  // the files below it; the directory must not be skipped.
  const dir = compileMatcher("**/*.log", { dot: true }).matchDir("x.log");
  assert.equal(dir, DirMatch.DescendAndMatch);
  assert.ok(DirMatch.matches(dir) && !DirMatch.matchesAllBelow(dir));
  // Where nothing below can be excluded, no more tests are needed under it.
  const built = compileMatcher(["dist/**", "build/**"], { dot: true });
  assert.equal(built.matchDir("src"), DirMatch.Pruned);
  assert.equal(built.matchDir("dist"), DirMatch.DescendAll);
}

// `dot: false` never covers a subtree: a dot-led name below escapes every wildcard.
assert.equal(
  compileMatcher("**/node_modules/**", { dot: false }).matchDir("node_modules"),
  DirMatch.Descend,
);

// A negated pattern is always worth walking.
assert.equal(compileMatcher("!**/*.ts").matchDir("src"), DirMatch.Descend);

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

console.log("✓ matchDir verdicts serve both the selecting and the excluding role");

// Windows: an escaped `\` is a literal no segment holds (§2.4), so its
// expansion seeds nothing, and `matchDir` prunes where it is next. Twin
// of `an_escaped_backslash_is_dead_on_windows`.
if (process.platform === "win32") {
  const text = (m) => m.staticPrefixes().map((b) => new TextDecoder().decode(b));
  assert.deepEqual(text(compileMatcher(["src\\\\sub/*.ts", "src/sub/*.ts"])), ["src/sub"]);
  assert.deepEqual(text(compileMatcher("src\\\\sub/*.ts")), []);
  assert.equal(compileMatcher("a/b\\\\c/*.ts").matchDir("a"), DirMatch.Pruned);
  console.log("✓ an escaped backslash is dead on Windows");
}
