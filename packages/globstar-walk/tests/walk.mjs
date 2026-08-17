// Integration tests for `glob` / `globSync` against real temp trees.
// Only the public API is exercised. Mirrors the shape of
// crates/globstar-walk/tests/walk.rs (the Rust twin), with extra rows
// for the JS-only surfaces (options normalization, error shapes).

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { glob, globSync, WalkError } from "../src/index.js";

let failures = 0;
const cleanups = [];

function tmpTree(tag, files, links = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), `glob-walk-js-${tag}-`));
  for (const rel of files) {
    const p = path.join(root, rel);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    if (rel.endsWith("/")) fs.mkdirSync(p, { recursive: true });
    else fs.writeFileSync(p, "x");
  }
  for (const [rel, target] of Object.entries(links)) {
    const p = path.join(root, rel);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.symlinkSync(target, p);
  }
  cleanups.push(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
}

const rel = (root, paths) => paths.map((p) => path.relative(root, p).replaceAll("\\", "/")).sort();

// Symlink creation needs a privilege on Windows — probe once, skip
// symlink-dependent assertions where unavailable.
const symlinksOk = (() => {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), "glob-walk-js-sym-"));
  try {
    fs.symlinkSync(d, path.join(d, "probe"));
    return true;
  } catch {
    return false;
  } finally {
    fs.rmSync(d, { recursive: true, force: true });
  }
})();

async function check(name, fn) {
  try {
    await fn();
    console.log(`  ok ${name}`);
  } catch (e) {
    failures++;
    console.error(`  FAIL ${name}: ${e.message}`);
  }
}

// Every sync expectation is also asserted against the async path.
async function expectBoth(root, patterns, options, expected, note) {
  const s = rel(root, globSync(patterns, { cwd: root, ...options }));
  assert.deepEqual(s, expected.slice().sort(), `${note} (sync)`);
  const a = rel(root, await glob(patterns, { cwd: root, ...options }));
  assert.deepEqual(a, expected.slice().sort(), `${note} (async)`);
}

// ── ignore must reach static-prefix seeds (2026-08 audit) ───────────────
await check("ignore applies to seeded prefixes", async () => {
  const root = tmpTree("ignoreseed", ["src/generated/api.ts", "src/index.ts"]);
  await expectBoth(
    root,
    ["src/generated/api.ts"],
    { ignore: ["**/generated/**"] },
    [],
    "seeded file",
  );
  await expectBoth(
    root,
    ["src/generated/api.ts", "!**/generated/**"],
    {},
    [],
    "auto-split negative",
  );
  await expectBoth(root, ["src/generated/*.ts"], { ignore: ["**/generated"] }, [], "seeded dir");
  await expectBoth(root, ["src/generated/api.ts"], { ignore: ["src"] }, [], "ignored ancestor");
  await expectBoth(
    root,
    ["src/**/*.ts"],
    { ignore: ["**/generated/**"] },
    ["src/index.ts"],
    "control",
  );
});

// ── `/!x` routes to ignore, not matcher negation (2026-08 audit) ────────
await check("slash-bang routes to ignore", async () => {
  const root = tmpTree("slashbang", ["a.txt", "b.txt"]);
  await expectBoth(root, ["**/*.txt", "/!b.txt"], {}, ["a.txt"], "acts as exclusion");
  await expectBoth(root, ["/!b.txt"], {}, [], "ignore-only yields nothing");
});

// ── broken symlink: seed ≡ walk (2026-08 audit) ─────────────────────────
await check("seed emits broken symlink like walk", async () => {
  if (!symlinksOk) return;
  const root = tmpTree("brokenseed", [], { "links/broken.txt": "/nonexistent-target-xyz" });
  await expectBoth(root, ["links/*.txt"], {}, ["links/broken.txt"], "walk path");
  await expectBoth(root, ["links/broken.txt"], {}, ["links/broken.txt"], "seed path");
});

// ── seed EACCES surfaces loudly (2026-08 audit) ─────────────────────────
await check("seed permission error throws", async () => {
  const root = tmpTree("eaccesseed", ["locked/b.txt"]);
  const locked = path.join(root, "locked");
  fs.chmodSync(locked, 0o000);
  try {
    let rootUser = true;
    try {
      fs.readdirSync(locked);
    } catch {
      rootUser = false;
    }
    if (rootUser) return; // permission wall is void under root
    assert.throws(
      () => globSync(["locked/b.txt"], { cwd: root }),
      (e) => e instanceof WalkError && e.kind === "Io",
    );
    await assert.rejects(
      glob(["locked/b.txt"], { cwd: root }),
      (e) => e instanceof WalkError && e.kind === "Io",
    );
  } finally {
    fs.chmodSync(locked, 0o755);
  }
});

// ── pattern normalization: ./ // and bare . (2026-08 audit) ─────────────
await check("patterns normalize ./ and //", async () => {
  const root = tmpTree("normalize", ["a/b/x.txt"]);
  for (const pat of ["./a/b/*.txt", "a//b/*.txt", "a/./b/*.txt", ".//a/b/*.txt"]) {
    await expectBoth(root, [pat], {}, ["a/b/x.txt"], `pattern ${pat} emits clean spelling`);
  }
  await expectBoth(root, ["."], {}, [], "bare `.` matches nothing");
  await expectBoth(root, ["a/b/*.txt", "//"], {}, ["a/b/x.txt"], "`//` member is dropped");
});

// ── looping symlink: seed ≡ walk ────────────────────────────────────────
await check("seed emits looping symlink like walk", async () => {
  if (!symlinksOk) return;
  const root = tmpTree("loopseed", [], { "links/loop.txt": "loop.txt" });
  await expectBoth(root, ["links/*.txt"], {}, ["links/loop.txt"], "walk path");
  await expectBoth(root, ["links/loop.txt"], {}, ["links/loop.txt"], "seed path");
});

// ── options: explicit undefined keeps defaults (2026-08 audit) ──────────
await check("undefined option values keep defaults", async () => {
  const root = tmpTree("optundef", ["real/y.txt"]);
  await expectBoth(
    root,
    ["real/*.txt"],
    { dot: undefined, ignore: undefined, caseInsensitive: undefined },
    ["real/y.txt"],
    "undefined option values keep defaults",
  );
  if (!symlinksOk) return;
  fs.symlinkSync(path.join(root, "real"), path.join(root, "links"));
  await expectBoth(
    root,
    ["links/*.txt"],
    { followSymlinks: undefined },
    ["links/y.txt"],
    "followSymlinks default survives explicit undefined",
  );
});

// ── option misuse: clear typed errors ───────────────────────────────────
await check("option misuse throws clear TypeErrors", async () => {
  const root = tmpTree("optmisuse", ["a.txt"]);
  await expectBoth(root, ["*.txt"], { ignore: "*.txt" }, [], "bare-string ignore accepted");
  assert.throws(() => globSync(["a.txt"], { cwd: root, ignore: 42 }), TypeError);
  assert.throws(() => globSync(["a.txt"], { cwd: root, ignore: ["ok", 5] }), TypeError);
  assert.throws(() => globSync(["a.txt"], { cwd: 42 }), TypeError);
  assert.throws(() => globSync(["a.txt"], { cwd: null }), TypeError);
  assert.throws(() => globSync(["a.txt"], { cwd: root, ignore: null }), TypeError);
  assert.throws(() => globSync([null], { cwd: root }), TypeError);
  assert.throws(() => globSync(42, { cwd: root }), TypeError);
});

// ── bad cwd fails loudly even with an empty positive set ────────────────
await check("bad cwd validated before empty-positives return", async () => {
  const missing = path.join(os.tmpdir(), "glob-walk-js-definitely-missing");
  assert.throws(
    () => globSync([], { cwd: missing }),
    (e) => e instanceof WalkError && e.kind === "Io",
  );
  assert.throws(
    () => globSync(["!x"], { cwd: missing }),
    (e) => e instanceof WalkError && e.kind === "Io",
  );
  await assert.rejects(
    glob([], { cwd: missing }),
    (e) => e instanceof WalkError && e.kind === "Io",
  );
});

// ── WalkError JSON shape ────────────────────────────────────────────────
await check("WalkError.toJSON is readable", async () => {
  const missing = path.join(os.tmpdir(), "glob-walk-js-definitely-missing");
  let err = null;
  try {
    globSync(["*"], { cwd: missing });
  } catch (e) {
    err = e;
  }
  const json = JSON.parse(JSON.stringify(err));
  assert.equal(json.name, "WalkError");
  assert.equal(json.kind, "Io");
  assert.equal(typeof json.message, "string");
  assert.equal(json.cause, undefined, "raw Node cause must not leak into JSON");
});

// ── walk basics: parity split, dot default, symlink cycle ───────────────
await check("walk basics still hold", async () => {
  const root = tmpTree("basics", ["src/a.ts", "src/a.test.ts", ".hidden/h.ts"]);
  await expectBoth(root, ["**/*.ts", "!**/*.test.ts"], {}, ["src/a.ts"], "bang split");
  await expectBoth(
    root,
    ["**/*.ts"],
    { dot: true },
    [".hidden/h.ts", "src/a.test.ts", "src/a.ts"],
    "dot=true",
  );
  if (!symlinksOk) return;
  const cyc = tmpTree("cycle", ["real/x.txt"]);
  fs.symlinkSync(path.join(cyc, "real"), path.join(cyc, "real", "back"));
  const got = rel(cyc, globSync(["**/*.txt"], { cwd: cyc }));
  assert.deepEqual(got, ["real/back/x.txt", "real/x.txt"].sort(), "cycle terminates");
});

for (const fn of cleanups) fn();
if (failures > 0) {
  console.error(`✗ ${failures} walker test group(s) failed`);
  process.exit(1);
}
console.log("✓ JS walker integration tests");
