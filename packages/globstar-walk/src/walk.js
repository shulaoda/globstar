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
// follows the link via `fs.statSync` to detect dir-vs-file targets, and
// breaks cycles by remembering, along each way down, the folders reached
// through a link (`linkToDescend`): a link to one of them is not
// descended, the rest of the walk continues. `followSymlinks: false`
// drops symlinks entirely (neither emitted nor descended; matches
// `fdir`'s `excludeSymlinks`).
//
// Errors are thrown, not swallowed, except where a root walk would see
// nothing (WALKER_SPEC §9.3, §9.4, §10): a folder removed or made a file
// after its parent listed it is left out. Compile failures land as
// `WalkError("InvalidPattern")`; readdir failures (EACCES, a missing
// cwd) land as `WalkError("Io")`. We diverge from tinyglobby (which
// suppresses IO errors) and fast-glob (which suppresses every ENOENT)
// on purpose: silent IO failures mask broken cwds and unreadable
// subtrees.

import * as fs from "node:fs";
import * as path from "node:path";
import { compileMatcher, DirMatch, GlobError } from "@globstar/core";
import { WalkError } from "./error.js";
import { normalizeOptions } from "./options.js";

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
        if (err && !gone(err)) {
          failed = true;
          reject(new WalkError("Io", { path: frame.absolute, cause: err }));
          return;
        }
        // Re-enter `submit` per descended dir so the next readdir
        // starts immediately — fans syscalls across libuv's threadpool.
        if (!err) processDirents(ctx, dirents, frame, out, descendShim);
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
      if (gone(cause)) continue;
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
  const matcherOpts = { dot: opts.dot, caseInsensitive: opts.caseInsensitive };

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

// Seed the traversal from the matcher's static prefixes. Each seed is
// reached the way a root walk reaches it: part by part from cwd, each
// part judged by `processDirents` as its folder's only entry (an lstat
// stands in for the dirent). An ancestor that gives no child frame drops
// the seed; the last part's file and frame are the seed's.
function initFromPrefixes(ctx) {
  for (const bytes of ctx.matcher.staticPrefixes()) {
    let frame = { absolute: ctx.cwd, relative: "", links: [] };
    const prefix = bytesToString(bytes);
    if (prefix === "") {
      ctx.seedFrames.push(frame);
      continue;
    }
    // A walked path never holds `..`, `.` or an empty segment, so no
    // root walk reaches such a prefix (and `..` would escape cwd).
    const parts = WINDOWS ? prefix.split(/[/\\]/) : prefix.split("/");
    if (parts.some((p) => p === "" || p === ".." || p === ".")) continue;
    for (let i = 0; i < parts.length && frame !== undefined; i++) {
      const name = parts[i];
      if (opensAsAnother(name) && !listed(frame.absolute, name)) break;
      const abs = withSep(frame.absolute) + name;
      let entry;
      try {
        entry = fs.lstatSync(abs);
      } catch (cause) {
        if (nothingThere(cause)) break; // §9.4
        throw new WalkError("Io", { path: abs, cause }); // §9.3
      }
      entry.name = name;
      const last = i === parts.length - 1;
      const below = [];
      processDirents(ctx, [entry], frame, last ? ctx.seedResults : [], below);
      if (last) ctx.seedFrames.push(...below);
      frame = below[0];
    }
  }
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
  const followSymlinks = ctx.followSymlinks;
  const links = frame.links;
  // A child is a head and its name; the folder's heads are made once.
  const relHead = frame.relative === "" ? "" : frame.relative + "/";
  const absHead = withSep(frame.absolute);

  for (let i = 0; i < dirents.length; i++) {
    const dirent = dirents[i];
    const isSymlink = dirent.isSymbolicLink();

    // `followSymlinks: false` → drop symlinks entirely (matches
    // tinyglobby's `excludeSymlinks` semantics, NOT walkdir's "treat
    // as file"). Skip before any further work.
    if (isSymlink && !followSymlinks) continue;

    const name = dirent.name;
    // A name that is not valid UTF-8 comes back with U+FFFD, a spelling
    // no file has: there is no path to report, nor a folder to read.
    if (name.includes("\uFFFD") && !exists(absHead + name)) continue;
    const childRel = relHead + name;

    // Ignore-filter early so an ignored symlink never costs a stat.
    if (ignore !== null && ignore.match(childRel)) continue;

    // A symlink takes its target's type (a Dirent only flags "is
    // symlink"); one that resolves to nothing (dangling, looping) stays
    // a link, emitted like a file.
    let target = null;
    if (isSymlink) {
      try {
        target = fs.statSync(absHead + name);
      } catch {
        // stays a link
      }
    }
    const typed = target ?? dirent;
    if (isSpecial(typed)) continue;

    // The absolute path is built only for an entry that needs it.
    if (!typed.isDirectory()) {
      if (matcher.match(childRel)) results.push(absHead + name);
      continue;
    }
    // Asked with its `/`, the folder is pruned by an ignore entry that
    // ends in `/` or `/**` (§3.2), under either `dot`.
    if (ignore !== null && ignore.match(childRel + "/")) continue;
    if (!DirMatch.mayMatchBelow(matcher.matchDir(childRel))) continue;
    // An ignore that covers everything below skips the subtree
    // (`**/x/**/*`; one that ends in `/**` was caught above).
    if (ctx.pruneIgnored && DirMatch.matchesAllBelow(ignore.matchDir(childRel))) continue;
    const childAbs = absHead + name;
    // Cycle break: a link to a folder already on the way down is not
    // descended.
    let childLinks = links;
    if (target !== null) {
      const link = linkToDescend(links, childAbs, target);
      if (link === null) continue;
      childLinks = links.concat(link);
    }
    descend.push({ absolute: childAbs, relative: childRel, links: childLinks });
  }
}

// The link `abs`, whose target `stat` typed, as an entry of a frame's
// `links`; `null` when descending it would close a cycle, its target
// being on the way down already. A folder has one `(dev, ino)`, which
// the stat gave for free; a hit is confirmed by canonical path, since
// some file systems hand out inode numbers that are not unique. Windows
// has no cheap identity in a stat: the canonical path is compared.
function linkToDescend(links, abs, stat) {
  if (WINDOWS) {
    const canonical = realpath(abs);
    if (canonical === null) return null;
    for (let i = 0; i < links.length; i++) if (links[i].canonical === canonical) return null;
    return { canonical };
  }
  const { dev, ino } = stat;
  let here;
  for (let i = 0; i < links.length; i++) {
    const link = links[i];
    if (link.dev !== dev || link.ino !== ino) continue;
    here ??= realpath(abs);
    if (here === null || here === realpath(link.path)) return null;
  }
  return { dev, ino, path: abs };
}

// The on-disk spelling of `p` (as Rust's `canonicalize`; the non-native
// `realpathSync` keeps a link's own spelling), or `null`.
function realpath(p) {
  try {
    return fs.realpathSync.native(p);
  } catch {
    return null;
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
// (GLOB_SPEC §6.2), that they share it. An error no one member explains
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

// A read error that says the folder is no longer there: removed, or
// replaced by a file, after its parent listed it — as if it had gone
// before (§9.3). cwd was checked first, so it never stands for a missing
// cwd.
const gone = (err) => err?.code === "ENOENT" || err?.code === "ENOTDIR";

// The entries of `dir`, typed by one lstat each; an entry that vanishes
// in between is left out, as if it had gone before the read.
function readEachEntry(dir) {
  const out = [];
  const head = withSep(dir);
  for (const name of fs.readdirSync(dir)) {
    let entry;
    try {
      entry = fs.lstatSync(head + name);
    } catch (err) {
      if (err?.code === "ENOENT") continue;
      throw err;
    }
    entry.name = name;
    out.push(entry);
  }
  return out;
}

// Can opening `name` reach an entry its folder lists under another
// name? A file system that ignores Unicode normalization (APFS) opens an
// NFC spelling of a name stored NFD; Win32 drops a trailing `.` or
// space, opens `NUL`, `con.txt` and the other device names as devices,
// and reads `C:` as a drive. A root walk, comparing names as listed,
// never matches such a spelling.
function opensAsAnother(name) {
  return isNonAscii(name) || (WINDOWS && win32Rewrites(name));
}

// Is `name` in the listing of `dir` as written? A folder that cannot be
// read leaves it to the lstat.
function listed(dir, name) {
  try {
    return fs.readdirSync(dir).includes(name);
  } catch {
    return true;
  }
}

// Does Win32 open this name as something else: a trailing `.` or space,
// a device name (`NUL`, `com1`, `con.txt`), or a `:` (a drive, a stream)?
function win32Rewrites(name) {
  if (name.endsWith(".") || name.endsWith(" ") || name.includes(":")) return true;
  return /^(?:CON|PRN|AUX|NUL|COM\d|LPT\d)[ \t\n\f\r]*$/i.test(name.split(".", 1)[0]);
}

// A FIFO, a socket or a device: not a file to read, so never emitted (a
// reader would block on a FIFO).
function isSpecial(s) {
  return !s.isDirectory() && !s.isFile() && !s.isSymbolicLink();
}

function isNonAscii(s) {
  for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) > 0x7f) return true;
  return false;
}

// §7.4: `parent` with a separator after it, so a child is `withSep(parent)
// + name` — bypassing `path.join`'s normalization (`..` / `.` / repeated
// `/`): we only ever append a dirent name, or a seed part the seeding
// checks for `..`, `.` and empty, to a `path.resolve`'d cwd or a previous
// child, so neither input ever holds those. The separator is `/` (§8.2),
// but `\` under a verbatim Windows path (`\\?\C:\…`), which Win32 reads
// with no `/` in it; a parent that already ends in one (a drive root,
// `C:\`) gets none.
function withSep(parent) {
  const last = parent.charCodeAt(parent.length - 1);
  if (last === 0x2f) return parent;
  if (!WINDOWS) return parent + "/";
  if (last === 0x5c) return parent;
  return parent + (parent.startsWith("\\\\?\\") ? "\\" : "/");
}

// Static prefixes are ASCII / valid UTF-8 bytes (parser-produced from
// a user pattern string), so plain Buffer decode is safe.
function bytesToString(bytes) {
  return Buffer.from(bytes).toString("utf8");
}
