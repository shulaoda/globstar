// `glob` (concurrent readdir) and `globSync` (depth-first) over
// `@globstar/core`; the behavior is `references/spec/WALKER_SPEC.md`.

import * as fs from "node:fs";
import * as path from "node:path";
import { compileMatcher, DirMatch, GlobError, MAX_PATTERN_LEN } from "@globstar/core";
import { WalkError } from "./error.js";
import { normalizeOptions, typeName } from "./options.js";

const WINDOWS = process.platform === "win32";

export async function glob(patterns, options) {
  const ctx = prepare(patterns, options);
  if (ctx === null) return [];
  const out = ctx.seedResults;
  if (ctx.seedFrames.length === 0) return out;

  return new Promise((resolve, reject) => {
    let pending = 0;
    let failed = false;
    const submit = (frame) => {
      pending++;
      fs.readdir(frame.absolute, { withFileTypes: true }, (err, dirents) => {
        pending--;
        if (failed) return;
        if (err) {
          try {
            dirents = afterReadError(err, frame.absolute);
          } catch (e) {
            failed = true;
            reject(e);
            return;
          }
        }
        // Each folder found is submitted at once: the reads fan out
        // across libuv's threadpool.
        if (dirents !== null) processDirents(ctx, dirents, frame, out, descendShim);
        if (pending === 0) resolve(out);
      });
    };
    const descendShim = { push: submit };

    for (let i = 0; i < ctx.seedFrames.length; i++) submit(ctx.seedFrames[i]);
  });
}

export function globSync(patterns, options) {
  const ctx = prepare(patterns, options);
  if (ctx === null) return [];

  const out = ctx.seedResults;
  // Popped: the first seed walks first, and a folder's first child next.
  const stack = ctx.seedFrames.slice().reverse();
  const descend = [];
  while (stack.length > 0) {
    const frame = stack.pop();
    let dirents;
    try {
      dirents = fs.readdirSync(frame.absolute, { withFileTypes: true });
    } catch (err) {
      dirents = afterReadError(err, frame.absolute);
      if (dirents === null) continue;
    }
    descend.length = 0;
    processDirents(ctx, dirents, frame, out, descend);
    for (let i = descend.length - 1; i >= 0; i--) stack.push(descend[i]);
  }
  return out;
}

function prepare(patterns, optsInput) {
  const opts = normalizeOptions(optsInput);
  const matcherOpts = { dot: opts.dot, caseInsensitive: opts.caseInsensitive };

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
      throw new TypeError(`pattern must be a string, got ${typeName(raw)}`);
    }
    checkLength(raw);
    const split = splitBangs(raw);
    if (split === null) continue;
    (split.bangs & 1 ? negatives : positives).push({ raw, body: split.body });
  }

  // Positives first: the same error precedence as the Rust walker.
  const matcher = compileUnion(positives, matcherOpts);
  const ignorePatterns = [];
  for (const raw of opts.ignore) {
    checkLength(raw);
    // An ignore entry is normalized but never `!`-split (§3.2); a
    // leading `!` there would ignore everything else.
    const n = normalizePattern(raw);
    if (n === null) continue;
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

  // Before the no-positives return: a bad cwd fails whatever the patterns.
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
    followSymlinks: opts.followSymlinks,
    // Whether the ignore set can cover a whole subtree, which `dot: false`
    // never does.
    pruneIgnored: opts.dot && ignore !== null,
    seedFrames: [],
    seedResults: [],
  };
  initFromPrefixes(ctx);
  return ctx;
}

// Each seed is reached the way a root walk reaches it: part by part from
// cwd, each part judged as its folder's only entry (an lstat stands in
// for the dirent). An ancestor that gives no child frame drops the seed.
function initFromPrefixes(ctx) {
  for (const bytes of ctx.matcher.staticPrefixes()) {
    let frame = { absolute: ctx.cwd, relative: "", links: [] };
    const prefix = Buffer.from(bytes).toString();
    if (prefix === "") {
      ctx.seedFrames.push(frame);
      continue;
    }
    // A walked path never holds `..`, `.` or an empty segment, so no root
    // walk reaches such a prefix.
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
        if (nothingThere(cause)) break;
        throw new WalkError("Io", { path: abs, cause });
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

// The entries of one folder. `results` and `descend` are push sinks: the
// sync walk passes arrays, the async one `out` and `{ push: submit }`.
function processDirents(ctx, dirents, frame, results, descend) {
  const matcher = ctx.matcher;
  const ignore = ctx.ignore;
  const followSymlinks = ctx.followSymlinks;
  const links = frame.links;
  const relHead = frame.relative === "" ? "" : frame.relative + "/";
  const absHead = withSep(frame.absolute);

  for (let i = 0; i < dirents.length; i++) {
    const dirent = dirents[i];
    const isSymlink = dirent.isSymbolicLink();
    if (isSymlink && !followSymlinks) continue;

    const name = dirent.name;
    // A name that is not valid UTF-8 comes back with U+FFFD, a spelling no
    // file has (§7.1).
    if (name.includes("\uFFFD") && !exists(absHead + name)) continue;
    const childRel = relHead + name;

    // Before any stat: an ignored symlink costs no resolution.
    if (ignore !== null && ignore.match(childRel)) continue;

    // A symlink takes its target's type; one that resolves to nothing
    // stays a link, yielded like a file.
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

    if (!typed.isDirectory()) {
      if (matcher.match(childRel)) results.push(absHead + name);
      continue;
    }
    // Asked with its `/`, a folder is pruned by an ignore entry that ends
    // in `/` or `/**` (§3.2), under either `dot`.
    if (ignore !== null && ignore.match(childRel + "/")) continue;
    if (!DirMatch.mayMatchBelow(matcher.matchDir(childRel))) continue;
    // An ignore that covers everything below skips the subtree.
    if (ctx.pruneIgnored && DirMatch.matchesAllBelow(ignore.matchDir(childRel))) continue;
    const childAbs = absHead + name;
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
// `links`; null when its target is on the way down already. A `(dev,
// ino)` hit is confirmed by canonical path, since some file systems hand
// out inode numbers that are not unique; Windows has no cheap identity in
// a stat, so the canonical path is compared.
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

// The on-disk spelling of `p` (the non-native `realpathSync` keeps a
// link's own spelling), or null.
function realpath(p) {
  try {
    return fs.realpathSync.native(p);
  } catch {
    return null;
  }
}

// The entries of `dir` after its read failed with `err`, or null when it
// is gone (§9.3); any other failure throws the `WalkError` naming `dir`.
function afterReadError(err, dir) {
  try {
    if (vanishedDuringRead(err)) return readEachEntry(dir);
  } catch (cause) {
    err = cause;
  }
  if (gone(err)) return null;
  throw new WalkError("Io", { path: dir, cause: err });
}

// Where a file system keeps no entry types (ext4 without `filetype`, XFS
// `ftype=0`, some FUSE and network mounts), readdir lstats each entry
// itself and fails the whole call when one vanishes between the two.
const vanishedDuringRead = (err) => err?.code === "ENOENT" && err?.syscall === "lstat";

// The entries of `dir`, typed by one lstat each; one that vanishes in
// between is left out.
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

// A read error that says the folder is no longer there: removed, or made
// a file, since its parent listed it (§9.3).
const gone = (err) => err?.code === "ENOENT" || err?.code === "ENOTDIR";

// A stat error that means no entry can be at the path, so a root walk
// would never list one (§9.4).
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

function exists(p) {
  try {
    fs.lstatSync(p);
    return true;
  } catch {
    return false;
  }
}

// A pattern spelled the way walked paths are (§4.3): leading `/`s
// stripped, `//` collapsed, `.` segments dropped, a last `.` kept as a
// trailing `/` (`d/.` ≡ `d/`). Null when nothing remains (`.`, `./`,
// `//`); the written "" stays, for the core to refuse.
function normalizePattern(raw) {
  let i = 0;
  while (i < raw.length && raw.charCodeAt(i) === 0x2f) i++;
  const s = raw.slice(i);
  const trailingSlash = s.endsWith("/") || s.endsWith("/.");
  const segs = s.split("/").filter((seg) => seg !== "" && seg !== ".");
  if (segs.length === 0) return raw === "" ? "" : null;
  return segs.join("/") + (trailingSlash ? "/" : "");
}

// The `!` count and the body of a written pattern (§4.2); null when it
// selects or ignores nothing (`.`, `//`, `!.`). Normalized first, so
// `/!x` ≡ `!x`; cutting a `!` run can leave only its own segment empty or
// `.`, which goes with its `/`, and a run after it counts too (`!/!x` ≡
// `!!x`).
function splitBangs(raw) {
  let rest = normalizePattern(raw);
  if (rest === null) return null;
  let bangs = 0;
  for (;;) {
    let run = 0;
    while (run < rest.length && rest.charCodeAt(run) === 0x21) run++;
    if (run === 0) return { bangs, body: rest };
    bangs += run;
    rest = rest.slice(run);
    if (rest === "") return { bangs, body: "" };
    const left = rest.startsWith(".") ? rest.slice(1) : rest;
    if (left === "" || left.startsWith("/")) {
      rest = left.slice(1);
      if (rest === "") return null;
    }
  }
}

// The `{ raw, body }` pairs as one matcher, or null for none. A failure
// names the member as written, with the text its offsets count in when
// normalizing changed it.
function compileUnion(patterns, opts) {
  if (patterns.length === 0) return null;
  try {
    return compileMatcher(
      patterns.map((p) => p.body),
      opts,
    );
  } catch (e) {
    if (!(e instanceof GlobError)) throw e;
    const { raw, body } = patterns[e.index ?? 0];
    let reason = e.message.replace(/^pattern \d+: /, "");
    if (raw !== body) reason += ` (in '${body}', as the walker reads it)`;
    throw new WalkError("InvalidPattern", { pattern: raw, reason, cause: e });
  }
}

// A written pattern longer than MAX_PATTERN_LEN bytes is refused before
// it is read (§9.2), named by its first characters. A UTF-16 unit takes 1
// to 3 bytes.
function checkLength(raw) {
  if (raw.length <= MAX_PATTERN_LEN / 3) return;
  const len = Buffer.byteLength(raw);
  if (len <= MAX_PATTERN_LEN) return;
  let head = "";
  let shown = 0;
  for (const ch of raw) {
    if (shown++ === 64) break;
    head += ch;
  }
  const cause = new GlobError("TooLong", { len, max: MAX_PATTERN_LEN });
  throw new WalkError("InvalidPattern", { pattern: `${head}…`, reason: cause.message, cause });
}

// Can opening `name` reach an entry its folder lists under another name?
// APFS opens an NFC spelling of a name stored NFD; Win32 drops a trailing
// `.` or space, opens the device names as devices, and reads `C:` as a
// drive.
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

// A trailing `.` or space, a device name (`NUL`, `com1`, `con.txt`), or a
// `:` (a drive, a stream).
function win32Rewrites(name) {
  if (name.endsWith(".") || name.endsWith(" ") || name.includes(":")) return true;
  return /^(?:CON|PRN|AUX|NUL|COM\d|LPT\d)[ \t\n\f\r]*$/i.test(name.split(".", 1)[0]);
}

// A FIFO, a socket or a device: a reader would block on a FIFO.
function isSpecial(s) {
  return !s.isDirectory() && !s.isFile() && !s.isSymbolicLink();
}

function isNonAscii(s) {
  for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) > 0x7f) return true;
  return false;
}

// `parent` with a separator after it (§7.4), so a child is `withSep(parent)
// + name` without `path.join`'s normalization: what is appended is a
// dirent name, or a seed part checked for `..`, `.` and empty. The
// separator is `/` (§8.2), but `\` under a verbatim Windows path
// (`\\?\C:\…`), which Win32 reads with no `/` in it.
function withSep(parent) {
  const last = parent.charCodeAt(parent.length - 1);
  if (last === 0x2f) return parent;
  if (!WINDOWS) return parent + "/";
  if (last === 0x5c) return parent;
  return parent + (parent.startsWith("\\\\?\\") ? "\\" : "/");
}
