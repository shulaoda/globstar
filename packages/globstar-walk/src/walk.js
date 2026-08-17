// Filesystem glob built on top of `globstar`.
//
//   glob(patterns, options?)      → Promise<string[]>      (concurrent readdir)
//   globSync(patterns, options?)  → string[]               (DFS readdirSync)
//
// `!`-prefixed entries auto-split into the ignore set:
//
//   glob(["**/*.ts", "!**/*.test.ts"])
//     ≡  glob(["**/*.ts"], { ignore: ["**/*.test.ts"] })
//
// Output is always **absolute** file paths, joined from the resolved
// `cwd` (locked at construction via `path.resolve`). Directory matches
// aren't emitted — files only. Callers that want paths relative to
// `cwd` can `path.relative(cwd, p)` at their boundary; this matches
// what Vite's `import.meta.glob` does internally (it always passes
// `absolute: true` to tinyglobby and post-relativizes itself).
//
// Symlinks: `followSymlinks: true` (default, matching tinyglobby)
// follows the link via `fs.statSync` to detect dir-vs-file targets,
// and breaks cycles by `fs.realpathSync`-ing each target and checking
// the ancestor chain — the offending descent is dropped, the rest of
// the walk continues. `followSymlinks: false` drops symlinks entirely
// (neither emitted nor descended; matches `fdir`'s `excludeSymlinks`).
//
// Errors are thrown, never swallowed. Compile failures land as
// `WalkError("InvalidPattern")`; readdir failures (EACCES, ENOENT,
// missing cwd) land as `WalkError("Io")`. We diverge from tinyglobby
// / fast-glob here on purpose: silent IO failures mask broken cwds
// and unreadable subtrees.

import * as fs from "node:fs";
import * as path from "node:path";
import { compileMatcher, DirMatch, GlobError } from "@globstar/core";
import { WalkError } from "./error.js";
import { normalizeOptions, toMatcherOptions } from "./options.js";

export async function glob(patterns, options) {
  const ctx = prepare(patterns, options);
  if (ctx === null) return [];

  return new Promise((resolve, reject) => {
    const out = ctx.seedResults;
    if (ctx.seedFrames.length === 0) {
      resolve(out);
      return;
    }

    let pending = 0;
    let failed = false;
    const submit = (frame) => {
      if (failed) return;
      pending++;
      fs.readdir(frame.absolute, { withFileTypes: true }, (err, dirents) => {
        pending--;
        if (failed) return; // late callback after rejection — drop it
        if (err) {
          failed = true;
          reject(new WalkError("Io", { path: frame.absolute, cause: err }));
          return;
        }
        // Re-enter `submit` per descended dir so the next readdir
        // starts immediately — fans syscalls across libuv's threadpool.
        processDirents(ctx, dirents, frame, out, descendShim);
        if (pending === 0) resolve(out);
      });
    };
    // `processDirents` calls `descend.push(frame)` per child to recurse;
    // wrapping `submit` in a push-shaped sink lets sync (real array)
    // and async (immediate fan-out) share the inner loop verbatim.
    const descendShim = { push: submit };

    for (let i = 0; i < ctx.seedFrames.length; i++) submit(ctx.seedFrames[i]);
  });
}

export function globSync(patterns, options) {
  const ctx = prepare(patterns, options);
  if (ctx === null) return [];

  const out = ctx.seedResults;
  // Reverse so `pop` drains seeds in forward order.
  const stack = ctx.seedFrames.slice().reverse();

  const descend = [];
  while (stack.length > 0) {
    const frame = stack.pop();

    let dirents;
    try {
      dirents = fs.readdirSync(frame.absolute, { withFileTypes: true });
    } catch (cause) {
      throw new WalkError("Io", { path: frame.absolute, cause });
    }

    descend.length = 0;
    processDirents(ctx, dirents, frame, out, descend);
    // Push descended frames in reverse so the next pop is the first
    // child — preserves DFS forward order per level.
    for (let i = descend.length - 1; i >= 0; i--) stack.push(descend[i]);
  }
  return out;
}

function prepare(patterns, optsInput) {
  const opts = normalizeOptions(optsInput);
  const matcherOpts = toMatcherOptions(opts);

  const list = Array.isArray(patterns) ? patterns : [patterns];
  const positives = [];
  const negatives = [];
  for (let i = 0; i < list.length; i++) {
    const raw = list[i];
    if (typeof raw !== "string") {
      throw new TypeError(`pattern must be a string, got ${raw === null ? "null" : typeof raw}`);
    }
    // Normalize before the `!`-parity split (§4.3 before §4.2, so `/!x`
    // ≡ `!x`), then re-normalize the body (`!/x` exposes a fresh `/`).
    // To match a literal leading `!`, escape it: `\!foo`.
    const n1 = normalizePattern(raw);
    if (n1 === "" && raw !== "") continue; // `.`, `//` — selects nothing
    let bangs = 0;
    while (bangs < n1.length && n1.charCodeAt(bangs) === 0x21 /* ! */) bangs++;
    const tail = bangs === 0 ? n1 : n1.slice(bangs);
    const body = bangs === 0 ? tail : normalizePattern(tail);
    if (body === "" && tail !== "") continue; // `!.` — ignores nothing
    (bangs & 1 ? negatives : positives).push(body);
  }

  // Positives first (error-precedence parity with Rust); ignore still
  // compiles before the no-positives early return.
  const matcher = compilePositive(positives, matcherOpts);
  const ignoreInput = typeof opts.ignore === "string" ? [opts.ignore] : opts.ignore;
  if (!Array.isArray(ignoreInput)) {
    throw new TypeError(
      `ignore must be a string or an array of strings, got ${
        opts.ignore === null ? "null" : typeof opts.ignore
      }`,
    );
  }
  const ignorePatterns = [];
  for (const raw of ignoreInput) {
    if (typeof raw !== "string") {
      throw new TypeError(
        `ignore pattern must be a string, got ${raw === null ? "null" : typeof raw}`,
      );
    }
    // §3.2: ignore entries are never `!`-split, only normalized.
    const n = normalizePattern(raw);
    if (n === "" && raw !== "") continue;
    ignorePatterns.push(n);
  }
  for (const n of negatives) ignorePatterns.push(n);
  const ignore = compilePositive(ignorePatterns, matcherOpts);

  // Lock cwd to an absolute path and validate BEFORE the empty-positives
  // early return: bad-cwd loudness must not depend on pattern content.
  if (typeof opts.cwd !== "string") {
    throw new TypeError(
      `cwd must be a string, got ${opts.cwd === null ? "null" : typeof opts.cwd}`,
    );
  }
  const cwd = path.resolve(opts.cwd);
  let cwdStat;
  try {
    cwdStat = fs.statSync(cwd);
  } catch (cause) {
    throw new WalkError("Io", { path: cwd, cause });
  }
  if (!cwdStat.isDirectory()) {
    throw new WalkError("Io", {
      path: cwd,
      cause: new Error("cwd is not a directory"),
    });
  }
  if (matcher === null) return null;

  const ctx = {
    matcher,
    ignore,
    cwd,
    followSymlinks: !!opts.followSymlinks,
    seedFrames: [],
    seedResults: [],
  };
  initFromPrefixes(ctx);
  return ctx;
}

// Seed the traversal from the matcher's static prefixes — pure-literal
// heads jump straight to their deepest known directory. Missing
// prefixes are silently skipped: `{a,b}/foo` where only `a/` exists
// still produces results from the `a` branch.
function initFromPrefixes(ctx) {
  for (const prefixBytes of ctx.matcher.staticPrefixes()) {
    if (prefixBytes.length === 0) {
      // Empty prefix = walk from cwd itself; cwd already validated.
      ctx.seedFrames.push({ absolute: ctx.cwd, relative: "", symlinkAncestors: [] });
      continue;
    }

    const prefixStr = bytesToString(prefixBytes);

    // `..` escapes cwd when joined and readdir yields neither `..` nor
    // `.`. Split on every platform separator so a Windows `..\secret`
    // can't slip past.
    const segs = WINDOWS ? prefixStr.split(/[/\\]/) : prefixStr.split("/");
    if (segs.some((seg) => seg === ".." || seg === ".")) continue;

    if (ctx.ignore !== null && seedIgnored(ctx.ignore, prefixStr)) continue;

    const joined = joinAbs(ctx.cwd, prefixStr);

    // `followSymlinks: false` drops symlinks entirely (§10). A root
    // walk would drop a symlink dirent and never reach a prefix on or
    // beyond it, so seeding must not jump through one either.
    if (!ctx.followSymlinks && seedCrossesSymlink(ctx.cwd, prefixStr)) continue;

    let stat;
    try {
      stat = fs.statSync(joined);
    } catch {
      // Still lstat-able ⇒ the entry exists (broken/looping symlink):
      // the walk emits those as non-directories (§10), so the seed
      // must too.
      try {
        fs.lstatSync(joined);
      } catch (lcause) {
        const code = lcause?.code;
        if (code === "ENOENT" || code === "ENOTDIR") continue; // §9.4
        // EACCES etc.: a root walk fails loudly here (§9.3).
        throw new WalkError("Io", { path: joined, cause: lcause });
      }
      if (ctx.matcher.match(prefixStr)) ctx.seedResults.push(joined);
      continue;
    }

    if (stat.isDirectory()) {
      // Walker only emits files; dir matchDir result just gates descend.
      if (DirMatch.shouldDescend(ctx.matcher.matchDir(prefixStr))) {
        ctx.seedFrames.push({ absolute: joined, relative: prefixStr, symlinkAncestors: [] });
      }
    } else if (ctx.matcher.match(prefixStr)) {
      ctx.seedResults.push(joined);
    }
  }
}

// An ignored directory prunes its subtree (§7.1 step 3), so a seed is
// dead if `ignore` matches it or any `/`-bounded ancestor.
function seedIgnored(ignore, prefixStr) {
  for (let i = 0; i < prefixStr.length; i++) {
    const c = prefixStr.charCodeAt(i);
    if ((c === 0x2f || (WINDOWS && c === 0x5c)) && ignore.match(prefixStr.slice(0, i))) {
      return true;
    }
  }
  return ignore.match(prefixStr);
}

// Does any component of `cwd + "/" + prefixStr` (from cwd down) resolve
// to a symlink? Used under `followSymlinks: false` to keep static-prefix
// seeding observationally equivalent to a root walk, which drops
// symlinks entirely (§10).
function seedCrossesSymlink(cwd, prefixStr) {
  let cur = cwd;
  for (const seg of WINDOWS ? prefixStr.split(/[/\\]/) : prefixStr.split("/")) {
    if (seg === "") continue;
    cur = joinAbs(cur, seg);
    try {
      if (fs.lstatSync(cur).isSymbolicLink()) return true;
    } catch {
      // Missing / inaccessible component — the later statSync handles it.
    }
  }
  return false;
}

// Per-frame match loop, shared by sync and async paths. `results` and
// `descend` are push-shaped sinks: sync passes plain arrays; async
// passes `out` and a `{ push: submit }` shim that fans new frames
// straight back into `fs.readdir`.
function processDirents(ctx, dirents, frame, results, descend) {
  const matcher = ctx.matcher;
  const ignore = ctx.ignore;
  const parentRel = frame.relative;
  const parentAbs = frame.absolute;
  const followSymlinks = ctx.followSymlinks;
  const ancestors = frame.symlinkAncestors;

  for (let i = 0; i < dirents.length; i++) {
    const dirent = dirents[i];
    const isSymlink = dirent.isSymbolicLink();

    // `followSymlinks: false` → drop symlinks entirely (matches
    // tinyglobby's `excludeSymlinks` semantics, NOT walkdir's "treat
    // as file"). Skip before any further work.
    if (isSymlink && !followSymlinks) continue;

    const name = dirent.name;
    const childRel = parentRel === "" ? name : parentRel + "/" + name;

    // Ignore-filter early so an ignored symlink never costs a stat.
    if (ignore !== null && ignore.match(childRel)) continue;

    let isDir = dirent.isDirectory();
    const childAbs = joinAbs(parentAbs, name);
    if (isSymlink) {
      // Node's Dirent only flags "is symlink", not the target type;
      // resolve on demand. (followSymlinks=true here since the off
      // case continued above.)
      try {
        isDir = fs.statSync(childAbs).isDirectory();
      } catch {
        isDir = false;
      }
    }

    if (isDir) {
      if (DirMatch.shouldDescend(matcher.matchDir(childRel))) {
        // Cycle break: when descending through a symlink target,
        // realpath and check against the ancestor chain. Already-
        // seen target → skip descent.
        let childAncestors;
        if (isSymlink) {
          let resolved;
          try {
            resolved = fs.realpathSync(childAbs);
          } catch {
            continue; // broken / inaccessible
          }
          if (ancestors.indexOf(resolved) !== -1) continue; // cycle
          childAncestors = ancestors.concat(resolved);
        } else {
          childAncestors = ancestors;
        }
        descend.push({
          absolute: childAbs,
          relative: childRel,
          symlinkAncestors: childAncestors,
        });
      }
    } else if (matcher.match(childRel)) {
      results.push(childAbs);
    }
  }
}

const WINDOWS = process.platform === "win32";

// WALKER_SPEC §4.3: spell patterns the way walked paths are spelled —
// strip leading `/`s, collapse `//`, drop `.` segments. `/` in a
// pattern is always structural (`\/` is a parse error), so textual
// splitting is safe. Returns "" when nothing remains (`.`, `./`).
function normalizePattern(s) {
  let i = 0;
  while (i < s.length && s.charCodeAt(i) === 0x2f) i++;
  if (i > 0) s = s.slice(i);
  if (!s.includes("/")) return s === "." ? "" : s;
  const trailingSlash = s.charCodeAt(s.length - 1) === 0x2f;
  const segs = s.split("/").filter((seg) => seg !== "" && seg !== ".");
  return segs.length === 0 ? "" : segs.join("/") + (trailingSlash ? "/" : "");
}

function compilePositive(patterns, opts) {
  // Patterns arrive already normalized (leading `/` stripped etc.).
  if (patterns.length === 0) return null;
  try {
    return compileMatcher(patterns, opts);
  } catch (e) {
    throw new WalkError("InvalidPattern", {
      pattern: patterns.join(","),
      reason: e instanceof GlobError ? e.message : String(e),
    });
  }
}

// Bypass `path.join`'s normalization (`..` / `.` / repeated `/`) — we
// only ever append a single dirent name to a `path.resolve`'d cwd or
// a previous `joinAbs` result, so neither input ever contains those.
function joinAbs(parent, child) {
  return parent.charCodeAt(parent.length - 1) === 0x2f ? parent + child : parent + "/" + child;
}

// Static prefixes are ASCII / valid UTF-8 bytes (parser-produced from
// a user pattern string), so plain Buffer decode is safe.
function bytesToString(bytes) {
  return Buffer.from(bytes).toString("utf8");
}
