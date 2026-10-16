// Integration tests for `glob` / `globSync` against real temp trees.
// Only the public API is exercised. Mirrors the shape of
// crates/globstar-walk/tests/walk.rs (the Rust twin), with extra rows
// for the JS-only surfaces (options normalization, error shapes).

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { syncBuiltinESMExports } from "node:module";
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

// ── InvalidPattern names the member that fails ──────────────────────────
await check("invalid member of a pattern list is named", async () => {
  const root = tmpTree("list-bad", ["src/a.ts"]);
  // The bad member is named, not the whole list: a comma-joined list
  // could read as one valid pattern (`a/{b,c/**}`).
  assert.throws(
    () => globSync(["src/**/*.{ts,tsx}", "lib/[abc"], { cwd: root }),
    (e) =>
      e instanceof WalkError &&
      e.kind === "InvalidPattern" &&
      e.pattern === "lib/[abc" &&
      e.reason.includes("unterminated character class at byte 4") &&
      e.cause?.kind === "UnterminatedClass",
  );
  assert.throws(
    () => globSync(["*.rs"], { cwd: root, ignore: ["a/{b", "c/**"] }),
    (e) => e instanceof WalkError && e.pattern === "a/{b",
  );
  // Every member compiles alone; together they pass the expansion budget
  // (GLOB_SPEC §7.7). Then the error lists them all.
  const member = "{,/}".repeat(10) + "**/f";
  const members = Array.from({ length: 10 }, (_, i) => `${member}${i}`);
  assert.throws(
    () => globSync(members, { cwd: root }),
    (e) =>
      e instanceof WalkError &&
      e.pattern === "[" + members.map((m) => JSON.stringify(m)).join(", ") + "]" &&
      e.cause?.kind === "BraceExpansionTooLarge",
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

// ── seeds reach what a root walk reaches (2026-10 audit) ────────────────
// Twins of the `walker_seed_*` tests in crates/globstar-walk/tests/walk.rs.
await check("a seed with an empty segment is dropped", async () => {
  // `{a,/b}` gives the prefix `/b`; `cwd + "/" + "/b"` used to be walked.
  const root = tmpTree("emptyseg", ["a/x", "a/b/x", "b/x"]);
  await expectBoth(root, ["{a,/b}/x"], {}, ["a/x"], "{a,/b}/x");
  await expectBoth(root, ["{/a,/b}/x"], {}, [], "{/a,/b}/x");
  // A written `//` is collapsed first (WALKER_SPEC §4.3).
  await expectBoth(root, ["{a//b,b}/x"], {}, ["a/b/x", "b/x"], "{a//b,b}/x");
  await expectBoth(root, ["{/src,x}/m.js", "a/x"], {}, ["a/x"], "no doubled seed");
});

await check("seed errors follow the root walk", async () => {
  // Nothing can be below a looping link, past a name too long for the
  // filesystem, or at a name with a NUL.
  if (!symlinksOk) return;
  const root = tmpTree("seederr", ["ok.txt"], { loop: "loop" });
  await expectBoth(root, ["{ok.txt,loop/x.txt}"], {}, ["ok.txt"], "looping link");
  await expectBoth(root, ["a".repeat(300) + "/*.txt"], {}, [], "name too long");
  await expectBoth(root, ["{ok.txt,a\0b/x.txt}"], {}, ["ok.txt"], "NUL");
});

await check("a seed through a symlink knows its cycle", async () => {
  // A root walk reaches `a/link` holding the link's target, so it does
  // not go round `a/link/link`; a seed must start the same way.
  if (!symlinksOk) return;
  const root = tmpTree("seedcycle", ["a/x.txt"], { "a/link": "." });
  for (const [seeded, rootWalk] of [
    ["a/link/**/*.txt", "*/link/**/*.txt"],
    ["a/link/link/**/*.txt", "*/link/link/**/*.txt"],
    ["a/link/link/x.txt", "*/link/link/x.txt"],
  ]) {
    assert.deepEqual(
      rel(root, globSync([seeded], { cwd: root })),
      rel(root, globSync([rootWalk], { cwd: root })),
      seeded,
    );
  }
  await expectBoth(root, ["a/link/**/*.txt"], {}, ["a/link/x.txt"], "one level");
});

await check("cycles are found by the on-disk spelling", async () => {
  // `realpathSync` keeps a link's spelling (`../A`), so on a
  // case-insensitive disk two links to one folder looked like two.
  if (!symlinksOk) return;
  const probe = tmpTree("caseprobe", ["X"]);
  if (!fs.existsSync(path.join(probe, "x"))) return; // case-sensitive disk
  const root = tmpTree("casecycle", ["a/x.txt"], { "a/up1": "../A", "a/up2": "../a" });
  await expectBoth(root, ["**/*.txt"], {}, ["a/up1/x.txt", "a/up2/x.txt", "a/x.txt"], "3 paths");
});

// ── paths, bases and errors spelled as the user wrote them ──────────────
await check("a trailing `.` names the folder", async () => {
  // `d/.` is the folder `d`: dropping the `.` made `*/.` match files.
  const root = tmpTree("trailingdot", ["x.txt", "d/y.txt", "d/e/z.txt"]);
  await expectBoth(root, ["*/."], {}, [], "*/.");
  await expectBoth(root, ["**/."], {}, [], "**/.");
  await expectBoth(
    root,
    ["**"],
    { ignore: ["*/."] },
    ["d/e/z.txt", "d/y.txt", "x.txt"],
    "ignore */.",
  );
});

await check("cwd resolves `..` as text", async () => {
  // `jump/..` is the folder holding `jump`, not the parent of its
  // target, and `..` never reaches the output (Rust does the same).
  if (!symlinksOk) return;
  const root = tmpTree("cwddotdot", ["top.txt", "real/inner/in.txt", "real/r.txt"], {
    jump: "real/inner",
  });
  assert.deepEqual(globSync(["*.txt"], { cwd: path.join(root, "jump", "..") }), [
    path.join(root, "top.txt"),
  ]);
});

await check("an empty cwd is the current folder", async () => {
  const root = tmpTree("emptycwd", ["here.txt"]);
  const before = process.cwd();
  process.chdir(root);
  try {
    assert.deepEqual(rel(process.cwd(), globSync(["*.txt"], { cwd: "" })), ["here.txt"]);
  } finally {
    process.chdir(before);
  }
});

await check("an error names the pattern as written", async () => {
  assert.throws(
    () => globSync(["src/**", "!src/[z-a]"]),
    (e) =>
      e instanceof WalkError &&
      e.pattern === "!src/[z-a]" &&
      e.reason.includes("byte 4") &&
      e.reason.includes("'src/[z-a]'"),
  );
  assert.throws(
    () => globSync(["src/[z-a]"]),
    (e) => e instanceof WalkError && !e.reason.includes("as the walker reads it"),
  );
});

await check("a subtree the ignore covers is not read", async () => {
  // Under dot=true `**/node_modules/**` covers all of `node_modules`, so
  // an unreadable `node_modules` raises nothing. Twin of
  // `walker_skips_a_subtree_the_ignore_covers`.
  const root = tmpTree("ignoreprune", ["src/a.js", "node_modules/p/f.js"]);
  const locked = path.join(root, "node_modules");
  fs.chmodSync(locked, 0o000);
  let readable = true;
  try {
    fs.readdirSync(locked);
  } catch {
    readable = false;
  }
  try {
    if (readable) return; // root reads anything
    const opts = { dot: true, ignore: ["**/node_modules/**"] };
    await expectBoth(root, ["**/*.js"], opts, ["src/a.js"], "pruned");
  } finally {
    fs.chmodSync(locked, 0o755);
  }
});

await check("a name that is not UTF-8 is skipped", async () => {
  // Linux keeps any bytes; readdir hands them back with U+FFFD, a path
  // no file has. (APFS refuses such names: nothing to test there.)
  const root = tmpTree("nonutf8", ["src/ok.txt"]);
  const bad = Buffer.concat([Buffer.from(root + "/caf"), Buffer.from([0xe9])]);
  try {
    fs.mkdirSync(bad);
    fs.writeFileSync(Buffer.concat([bad, Buffer.from("/inner.txt")]), "x");
  } catch {
    return;
  }
  await expectBoth(root, ["**/*.txt"], {}, ["src/ok.txt"], "**/*.txt");
  await expectBoth(root, ["*/*.txt"], {}, ["src/ok.txt"], "*/*.txt");
});

await check("an entry that vanishes during a typed read is skipped", async () => {
  // Without entry types (ext4 `^filetype`, XFS `ftype=0`, …) readdir
  // lstats each entry itself and fails when one vanishes in between;
  // simulate that once per call.
  const root = tmpTree("vanish", ["src/a.ts", "src/b.ts"]);
  const vanished = () =>
    Object.assign(new Error("ENOENT: no such file or directory, lstat"), {
      code: "ENOENT",
      syscall: "lstat",
    });
  const fsMod = (await import("node:fs")).default;
  const real = { readdirSync: fsMod.readdirSync, readdir: fsMod.readdir };
  try {
    let failSync = true;
    fsMod.readdirSync = (dir, opts) => {
      if (opts?.withFileTypes && failSync) {
        failSync = false;
        throw vanished();
      }
      return real.readdirSync(dir, opts);
    };
    let failAsync = true;
    fsMod.readdir = (dir, opts, cb) => {
      if (opts?.withFileTypes && failAsync) {
        failAsync = false;
        process.nextTick(cb, vanished());
        return;
      }
      real.readdir(dir, opts, cb);
    };
    syncBuiltinESMExports();
    await expectBoth(root, ["**/*.ts"], {}, ["src/a.ts", "src/b.ts"], "fallback");
  } finally {
    fsMod.readdirSync = real.readdirSync;
    fsMod.readdir = real.readdir;
    syncBuiltinESMExports();
  }
});

for (const fn of cleanups) fn();
if (failures > 0) {
  console.error(`✗ ${failures} walker test group(s) failed`);
  process.exit(1);
}
console.log("✓ JS walker integration tests");
