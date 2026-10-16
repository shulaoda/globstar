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
// and breaks cycles by `fs.realpathSync.native`-ing each target (the
// on-disk spelling, as Rust's `canonicalize`) and checking the ancestor
// chain — the offending descent is dropped, the rest of
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
        if (err && vanishedDuringRead(err)) {
          try {
            dirents = readEachEntry(frame.absolute);
            err = null;
          } catch (cause) {
            err = cause;
          }
        }
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
      try {
        dirents = fs.readdirSync(frame.absolute, { withFileTypes: true });
      } catch (err) {
        if (!vanishedDuringRead(err)) throw err;
        dirents = readEachEntry(frame.absolute);
      }
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

  // A string is one pattern; any other iterable is a list.
  const list =
    typeof patterns !== "string" &&
    patterns != null &&
    typeof patterns[Symbol.iterator] === "function"
      ? [...patterns]
      : [patterns];
  // Each body keeps the text it came from, for error messages.
  const positives = [];
  const negatives = [];
  for (let i = 0; i < list.length; i++) {
    const raw = list[i];
    if (typeof raw !== "string") {
      throw new TypeError(`pattern must be a string, got ${raw === null ? "null" : typeof raw}`);
    }
    // To match a literal leading `!`, escape it: `\!foo`.
    const split = splitBangs(raw);
    if (split === null) continue; // `.`, `//`, `!.` — selects or ignores nothing
    (split.bangs & 1 ? negatives : positives).push({ raw, body: split.body });
  }

  // Positives first (error-precedence parity with Rust); ignore still
  // compiles before the no-positives early return.
  const matcher = compileUnion(positives, matcherOpts);
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
    // §3.2: ignore entries are never `!`-split, only normalized. A
    // leading `!` there would ignore everything else, and is refused.
    const n = normalizePattern(raw);
    if (n === "" && raw !== "") continue;
    if (n.startsWith("!")) {
      throw new WalkError("InvalidPattern", {
        pattern: raw,
        reason:
          "an ignore entry cannot start with `!` (it would ignore everything else); " +
          "list what to ignore, and write `\\!` for a name that starts with `!`",
      });
    }
    ignorePatterns.push({ raw, body: n });
  }
  for (const n of negatives) ignorePatterns.push(n);
  const ignore = compileUnion(ignorePatterns, matcherOpts);

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
    // Whether the ignore set can cover a whole subtree (matchDir
    // all-below), which `dot: false` never does.
    pruneIgnored: !!opts.dot && ignore !== null,
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

    // A walked path never holds `..`, `.` or an empty segment (readdir
    // yields none of them), so a prefix with one (`{a,/b}` gives `/b`)
    // is reached by no root walk; `..` would also escape cwd. Split on
    // every platform separator so a Windows `..\secret` can't slip past.
    const segs = WINDOWS ? prefixStr.split(/[/\\]/) : prefixStr.split("/");
    if (segs.some((seg) => seg === "" || seg === ".." || seg === ".")) continue;

    if (ctx.ignore !== null && seedIgnored(ctx.ignore, prefixStr)) continue;

    // Part by part, so the seed is joined as a root walk's paths are.
    let joined = ctx.cwd;
    for (const seg of segs) joined = joinAbs(joined, seg);

    // The links a root walk would follow on the way down.
    const links = seedLinks(ctx, segs);
    if (links === null) continue;

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
        if (nothingThere(lcause)) continue; // §9.4
        // EACCES etc.: a root walk fails loudly here (§9.3).
        throw new WalkError("Io", { path: joined, cause: lcause });
      }
      if (ctx.matcher.match(prefixStr)) ctx.seedResults.push(joined);
      continue;
    }

    if (stat.isDirectory()) {
      // Walker only emits files; dir matchDir result just gates descend.
      if (links.descend && DirMatch.mayMatchBelow(ctx.matcher.matchDir(prefixStr))) {
        ctx.seedFrames.push({
          absolute: joined,
          relative: prefixStr,
          symlinkAncestors: links.chain,
        });
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

// What a root walk knows on reaching the seed `segs`: the targets of
// the symlinks it followed on the way (the frame's cycle chain, §10),
// and whether it would descend into the seed; `null` when it never gets
// there — a symlink under `followSymlinks: false`, which drops them
// entirely, or a link back to one already followed. A missing or broken
// part ends the scan: the seed's own stat decides.
function seedLinks(ctx, segs) {
  const chain = [];
  let cur = ctx.cwd;
  for (let i = 0; i < segs.length; i++) {
    cur = joinAbs(cur, segs[i]);
    let target;
    try {
      if (!fs.lstatSync(cur).isSymbolicLink()) continue;
      if (!ctx.followSymlinks) return null;
      target = fs.realpathSync.native(cur);
    } catch {
      break;
    }
    if (chain.includes(target)) {
      // A cycle: the walk stops descending here, so it reaches the seed
      // only when the seed is this link.
      return i === segs.length - 1 ? { chain, descend: false } : null;
    }
    chain.push(target);
  }
  return { chain, descend: true };
}

// A stat error that means no entry can be at the path, so a root walk
// would never list one (§9.4): it is missing, a part is not a folder, a
// link loops, the name is too long or invalid, or it holds a NUL.
function nothingThere(err) {
  switch (err?.code) {
    case "ENOENT":
    case "ENOTDIR":
    case "ELOOP":
    case "ENAMETOOLONG":
    case "EINVAL":
    case "ERR_INVALID_ARG_VALUE":
      return true;
    default:
      return false;
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
    const childAbs = joinAbs(parentAbs, name);
    // A name that is not valid UTF-8 comes back with U+FFFD, a spelling
    // no file has: there is no path to report, nor a folder to read.
    if (name.includes("\uFFFD") && !exists(childAbs)) continue;
    const childRel = parentRel === "" ? name : parentRel + "/" + name;

    // Ignore-filter early so an ignored symlink never costs a stat.
    if (ignore !== null && ignore.match(childRel)) continue;

    let isDir = dirent.isDirectory();
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
      // An ignore that covers everything below skips the subtree
      // (`**/node_modules/**` and `node_modules`).
      const covered = ctx.pruneIgnored && DirMatch.matchesAllBelow(ignore.matchDir(childRel));
      if (!covered && DirMatch.mayMatchBelow(matcher.matchDir(childRel))) {
        // Cycle break: when descending through a symlink target,
        // realpath and check against the ancestor chain. Already-
        // seen target → skip descent.
        let childAncestors;
        if (isSymlink) {
          let resolved;
          try {
            resolved = fs.realpathSync.native(childAbs);
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
// strip leading `/`s, collapse `//`, drop `.` segments. A last `.`
// names the folder before it, so it becomes a trailing `/` (`d/.` ≡
// `d/`, never the files `d` would match). `/` in a pattern is always
// structural (`\/` is a parse error), so textual splitting is safe.
// Returns "" when nothing remains (`.`, `./`).
function normalizePattern(s) {
  let i = 0;
  while (i < s.length && s.charCodeAt(i) === 0x2f) i++;
  if (i > 0) s = s.slice(i);
  if (!s.includes("/")) return s === "." ? "" : s;
  const trailingSlash = s.endsWith("/") || s.endsWith("/.");
  const segs = s.split("/").filter((seg) => seg !== "" && seg !== ".");
  return segs.length === 0 ? "" : segs.join("/") + (trailingSlash ? "/" : "");
}

// WALKER_SPEC §4.2: the `!` count and the body of a written pattern;
// `null` when it selects or ignores nothing (`.`, `//`, `!.`).
// Normalization runs before the split (§4.3 first, so `/!x` ≡ `!x`) and
// again after each `!` run, which can expose a `/` and then another
// run: `!/!x` ≡ `!!x`, the body `x` with two `!`.
function splitBangs(raw) {
  let body = normalizePattern(raw);
  if (body === "" && raw !== "") return null;
  let bangs = 0;
  for (;;) {
    let run = 0;
    while (run < body.length && body.charCodeAt(run) === 0x21 /* ! */) run++;
    if (run === 0) return { bangs, body };
    bangs += run;
    const tail = body.slice(run);
    body = normalizePattern(tail);
    if (body === "" && tail !== "") return null;
  }
}

// Compiles `{ raw, body }` pairs (`body` normalized: leading `/`
// stripped etc.) into one matcher; `null` for an empty list. A failure
// names the member, as written, that the union failed in; when the
// written text differs, the reason says which text its byte offsets
// count in, and when it ran past the expansion budget the members share
// (GLOB_SPEC §7.7), that they share it. An error no one member explains
// (the union's compile work) names the list, cut after three.
function compileUnion(patterns, opts) {
  if (patterns.length === 0) return null;
  try {
    return compileMatcher(
      patterns.map((p) => p.body),
      opts,
    );
  } catch (e) {
    if (!(e instanceof GlobError)) throw e;
    // The core names the member it failed in (`index`, with offsets into
    // that member); an error no one member explains (the union's compile
    // work) is about the list.
    const at = e.index ?? (patterns.length === 1 ? 0 : null);
    if (at === null) {
      throw new WalkError("InvalidPattern", {
        pattern: nameList(patterns),
        reason: e.message,
        cause: e,
      });
    }
    const { raw, body } = patterns[at];
    let reason = e.message.replace(/^pattern \d+: /, "");
    if (e.kind === "BraceExpansionTooLarge" && patterns.length > 1) {
      reason += " (the patterns share this limit, and it ran out in this one)";
    }
    if (raw !== body) reason += ` (in '${body}', as the walker reads it)`;
    throw new WalkError("InvalidPattern", { pattern: raw, reason, cause: e });
  }
}

// A list of patterns as one name, cut after the first few.
function nameList(patterns) {
  const shown = patterns.slice(0, 3).map((p) => JSON.stringify(p.raw));
  const more = patterns.length - shown.length;
  return `[${shown.join(", ")}]` + (more > 0 ? ` and ${more} more` : "");
}

// Does an entry exist at `p`?
function exists(p) {
  try {
    fs.lstatSync(p);
    return true;
  } catch {
    return false;
  }
}

// Where a filesystem keeps no entry types (ext4 without `filetype`, XFS
// `ftype=0`, some FUSE and network mounts), readdir lstats each entry
// itself and fails the whole call when one vanishes between the two.
const vanishedDuringRead = (err) => err?.code === "ENOENT" && err?.syscall === "lstat";

// The entries of `dir`, typed by one lstat each; an entry that vanishes
// in between is left out, as if it had gone before the read.
function readEachEntry(dir) {
  const out = [];
  for (const name of fs.readdirSync(dir)) {
    let entry;
    try {
      entry = fs.lstatSync(joinAbs(dir, name));
    } catch (err) {
      if (err?.code === "ENOENT") continue;
      throw err;
    }
    entry.name = name;
    out.push(entry);
  }
  return out;
}

// §7.4: `parent`, a separator, `child`, bypassing `path.join`'s
// normalization (`..` / `.` / repeated `/`) — we only ever append a
// dirent name, or a seed part the seeding checks for `..`, `.` and
// empty, to a `path.resolve`'d cwd or a previous `joinAbs` result, so
// neither input ever holds those. The separator is `/` (§8.2), but `\`
// under a verbatim Windows path (`\\?\C:\…`), which Win32 reads with no
// `/` in it; a parent that already ends in one (a drive root, `C:\`)
// gets none.
function joinAbs(parent, child) {
  const last = parent.charCodeAt(parent.length - 1);
  if (last === 0x2f) return parent + child;
  if (!WINDOWS) return parent + "/" + child;
  if (last === 0x5c) return parent + child;
  return parent + (parent.startsWith("\\\\?\\") ? "\\" : "/") + child;
}

// Static prefixes are ASCII / valid UTF-8 bytes (parser-produced from
// a user pattern string), so plain Buffer decode is safe.
function bytesToString(bytes) {
  return Buffer.from(bytes).toString("utf8");
}
