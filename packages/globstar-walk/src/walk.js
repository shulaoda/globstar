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

  const matcher = compileUnion(positives, matcherOpts);
  const ignorePatterns = [];
  for (const raw of opts.ignore) {
    checkLength(raw);
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
    pruneIgnored: opts.dot && ignore !== null,
    seedFrames: [],
    seedResults: [],
  };
  initFromPrefixes(ctx);
  return ctx;
}

function initFromPrefixes(ctx) {
  for (const bytes of ctx.matcher.staticPrefixes()) {
    let frame = { absolute: ctx.cwd, relative: "", links: [] };
    const prefix = Buffer.from(bytes).toString();
    if (prefix === "") {
      ctx.seedFrames.push(frame);
      continue;
    }
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
    if (name.includes("\uFFFD") && !exists(absHead + name)) continue;
    const childRel = relHead + name;

    if (ignore !== null && ignore.match(childRel)) continue;

    let target = null;
    if (isSymlink) {
      try {
        target = fs.statSync(absHead + name);
      } catch {}
    }
    const typed = target ?? dirent;
    if (isSpecial(typed)) continue;

    if (!typed.isDirectory()) {
      if (matcher.match(childRel)) results.push(absHead + name);
      continue;
    }
    if (ignore !== null && ignore.match(childRel + "/")) continue;
    if (!DirMatch.mayMatchBelow(matcher.matchDir(childRel))) continue;
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

function realpath(p) {
  try {
    return fs.realpathSync.native(p);
  } catch {
    return null;
  }
}

function afterReadError(err, dir) {
  try {
    if (vanishedDuringRead(err)) return readEachEntry(dir);
  } catch (cause) {
    err = cause;
  }
  if (gone(err)) return null;
  throw new WalkError("Io", { path: dir, cause: err });
}

const vanishedDuringRead = (err) => err?.code === "ENOENT" && err?.syscall === "lstat";

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

const gone = (err) => err?.code === "ENOENT" || err?.code === "ENOTDIR";

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

function normalizePattern(raw) {
  let i = 0;
  while (i < raw.length && raw.charCodeAt(i) === 0x2f) i++;
  const s = raw.slice(i);
  const trailingSlash = s.endsWith("/") || s.endsWith("/.");
  const segs = s.split("/").filter((seg) => seg !== "" && seg !== ".");
  if (segs.length === 0) return raw === "" ? "" : null;
  return segs.join("/") + (trailingSlash ? "/" : "");
}

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

function opensAsAnother(name) {
  return isNonAscii(name) || (WINDOWS && win32Rewrites(name));
}

function listed(dir, name) {
  try {
    return fs.readdirSync(dir).includes(name);
  } catch {
    return true;
  }
}

function win32Rewrites(name) {
  if (name.endsWith(".") || name.endsWith(" ") || name.includes(":")) return true;
  return /^(?:CON|PRN|AUX|NUL|COM\d|LPT\d)[ \t\n\f\r]*$/i.test(name.split(".", 1)[0]);
}

function isSpecial(s) {
  return !s.isDirectory() && !s.isFile() && !s.isSymbolicLink();
}

function isNonAscii(s) {
  for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) > 0x7f) return true;
  return false;
}

function withSep(parent) {
  const last = parent.charCodeAt(parent.length - 1);
  if (last === 0x2f) return parent;
  if (!WINDOWS) return parent + "/";
  if (last === 0x5c) return parent;
  return parent + (parent.startsWith("\\\\?\\") ? "\\" : "/");
}
