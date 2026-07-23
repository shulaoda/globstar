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
    const s = String(list[i]);
    // Strip leading `!`s and route by parity (matches the matcher's
    // parser, which collapses N negations into N % 2). To match a
    // filename literally starting with `!`, escape: `\!foo` / `[!]foo`.
    let bangs = 0;
    while (bangs < s.length && s.charCodeAt(bangs) === 0x21 /* ! */) bangs++;
    (bangs & 1 ? negatives : positives).push(bangs === 0 ? s : s.slice(bangs));
  }

  const matcher = compilePositive(positives, matcherOpts);
  if (matcher === null) return null;
  const ignore = compilePositive([...opts.ignore, ...negatives], matcherOpts);

  // Lock cwd to an absolute path so `process.chdir` after construction
  // doesn't redirect the walk. Validate up front: a bad cwd should
  // fail loudly here, not produce a confusing empty result.
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
    const joined = joinAbs(ctx.cwd, prefixStr);

    let stat;
    try {
      stat = fs.statSync(joined);
    } catch {
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

function compilePositive(patterns, opts) {
  // Strip leading `/` so static prefixes always come out relative to cwd.
  const stripped = patterns.map((p) => p.replace(/^\/+/, ""));
  if (stripped.length === 0) return null;
  try {
    return compileMatcher(stripped, opts);
  } catch (e) {
    throw new WalkError("InvalidPattern", {
      pattern: stripped.join(","),
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
