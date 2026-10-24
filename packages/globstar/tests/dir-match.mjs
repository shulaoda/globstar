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

// `?` takes every one-byte name and `??*` every longer one: side by side
// they cover every name, though neither does alone, in every NFA kind
// (padding grows the graph past each kind's size). Twin of
// `branches_that_split_names_by_length_cover_together`.
{
  const below = (p, dir) =>
    DirMatch.matchesAllBelow(compileMatcher(p, { dot: true }).matchDir(dir));
  for (const pad of [0, 20, 60, 200]) {
    const pads = Array.from({ length: pad }, (_, i) => `,p${i}/x`).join("");
    assert.ok(below(`{**/?,**/??*${pads}}`, ""), pad);
    assert.ok(below(`{**/?,**/??*${pads}}`, "a/bc"), pad);
    assert.ok(below(`{?/**/?,**/??*${pads}}`, "a"), pad);
    // No branch takes two bytes.
    assert.ok(!below(`{**/?,**/???*${pads}}`, "a"), pad);
  }
  // Lengths are told apart up to 31 bytes, every longer one as one.
  const q = (n) => "?".repeat(n);
  const upto = (k, skip) =>
    Array.from({ length: k }, (_, i) => i + 1)
      .filter((n) => n !== skip)
      .map((n) => `**/${q(n)},`)
      .join("");
  assert.ok(below(`{${upto(31, 0)}**/${q(32)}*}`, ""));
  assert.ok(!below(`{${upto(31, 17)}**/${q(32)}*}`, ""));
  assert.ok(!below(`{${upto(30, 0)}**/${q(32)}*}`, ""));
  // A run of 33 is past the classes: not provable, though it covers.
  assert.ok(!below(`{${upto(31, 0)}**/${q(32)},**/${q(33)}*}`, ""));
}

// A covering pair beside many `?` runs, so that every length class fires
// sets of its own: the search gives up within its caps. Twin of
// `the_all_below_search_by_length_stays_cheap`.
{
  let seed = 7;
  const rnd = (n) => {
    seed = (Math.imul(seed, 1103515245) + 12345) & 0x7fffffff;
    return seed % n;
  };
  const branches = ["**/?", "**/??*"];
  for (let i = 0; i < 400; i++) {
    const depth = 1 + rnd(6);
    const segs = Array.from({ length: depth }, () => "?".repeat(1 + rnd(31)));
    branches.push(`**/${segs.join("/")}/z${i}`);
  }
  const m = compileMatcher(`{${branches.join(",")}}`, { dot: true });
  const start = performance.now();
  for (let k = 0; k < 50; k++) {
    for (const dir of ["", "a", "ab/c", "abcdefgh/x/yy"]) {
      assert.ok(DirMatch.mayMatchBelow(m.matchDir(dir)));
    }
  }
  assert.ok(performance.now() - start < 2000, `${performance.now() - start} ms`);
}

console.log("✓ branches that split names by length cover together, cheaply");

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
