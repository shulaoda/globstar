// Harvest the cases a Rust library's own tests assert, by reading the
// test source: `matches!`/`nmatches!`/`syntaxerr!` in globset's glob.rs,
// `assert!(glob_match(..))` in fast-glob's test.rs, `Glob::new(..)` and
// `is_match(..)` in wax's lib.rs.

import { readFileSync } from "node:fs";
import { join } from "node:path";

// A Rust string literal starting at `src[i]` (`"…"`, `r"…"`, `r#"…"#`):
// its value and the index after it, or null.
function literalAt(src, i) {
  let hashes = 0;
  let j = i;
  if (src[j] === "r") {
    j++;
    while (src[j] === "#") {
      hashes++;
      j++;
    }
    if (src[j] !== '"') return null;
    const close = '"' + "#".repeat(hashes);
    const end = src.indexOf(close, j + 1);
    if (end === -1) return null;
    return { value: src.slice(j + 1, end), end: end + close.length };
  }
  if (src[j] !== '"') return null;
  let out = "";
  for (j++; j < src.length; j++) {
    const c = src[j];
    if (c === '"') return { value: out, end: j + 1 };
    if (c !== "\\") {
      out += c;
      continue;
    }
    const n = src[++j];
    if (n === "n") out += "\n";
    else if (n === "t") out += "\t";
    else if (n === "r") out += "\r";
    else if (n === "0") out += "\0";
    else if (n === "u" && src[j + 1] === "{") {
      const end = src.indexOf("}", j);
      out += String.fromCodePoint(parseInt(src.slice(j + 2, end), 16));
      j = end;
    } else if (n === "x") {
      out += String.fromCharCode(parseInt(src.slice(j + 1, j + 3), 16));
      j += 2;
    } else out += n; // `\\`, `\"`, `\'`
  }
  return null;
}

const skipSpace = (src, i) => {
  while (i < src.length && /[\s,]/.test(src[i])) i++;
  return i;
};

// The name of the `fn` whose body holds index `i`.
function enclosingFn(src, i) {
  const before = src.slice(0, i);
  const m = [...before.matchAll(/fn\s+([a-z_0-9]+)\s*\(/g)].pop();
  return m ? m[1] : "";
}

const lineOf = (src, i) => src.slice(0, i).split("\n").length;

function makeCase(pattern, path, options, at, group, kind = "match") {
  return { pattern, path, options, at, group, kind };
}

// ── globset ───────────────────────────────────────────────────────────

// `matches!(name, pat, path[, OPTS])`, `nmatches!(…)` and
// `syntaxerr!(name, pat, kind)`; OPTS is one of the test module's option
// constants, kept by name for the generator to map or drop.
export function harvestGlobset(dir) {
  const file = "src/glob.rs";
  const src = readFileSync(join(dir, file), "utf8");
  const cases = [];
  const re = /\b(n?matches|syntaxerr)!\(\s*([a-z_0-9]+)\s*,\s*/g;
  for (const m of src.matchAll(re)) {
    const macro = m[1];
    let i = m.index + m[0].length;
    const pat = literalAt(src, i);
    if (!pat) continue;
    i = skipSpace(src, pat.end);
    const at = `${file}:${lineOf(src, m.index)}`;
    if (macro === "syntaxerr") {
      cases.push(makeCase(pat.value, "-", {}, at, m[2], "reject"));
      continue;
    }
    const path = literalAt(src, i);
    if (!path) continue;
    i = skipSpace(src, path.end);
    const opt = src.slice(i).match(/^([A-Z]+)\s*\)/);
    cases.push(makeCase(pat.value, path.value, { globset: opt ? opt[1] : null }, at, m[2]));
  }
  return { cases, skipped: {}, failures: 0, files: 1 };
}

// ── fast-glob ─────────────────────────────────────────────────────────

// `assert!(glob_match(pat, path))` and its negation, grouped by test fn.
export function harvestFastGlob(dir) {
  const file = "tests/test.rs";
  const src = readFileSync(join(dir, file), "utf8");
  const cases = [];
  const re = /assert!\(\s*!?\s*glob_match\(\s*/g;
  for (const m of src.matchAll(re)) {
    const pat = literalAt(src, m.index + m[0].length);
    if (!pat) continue;
    const path = literalAt(src, skipSpace(src, pat.end));
    if (!path) continue;
    cases.push(
      makeCase(
        pat.value,
        path.value,
        {},
        `${file}:${lineOf(src, m.index)}`,
        enclosingFn(src, m.index),
      ),
    );
  }
  return { cases, skipped: {}, failures: 0, files: 1 };
}

// ── wax ───────────────────────────────────────────────────────────────

// `let glob = Glob::new(pat).unwrap();` followed by
// `assert!(glob.is_match(Path::new(path)))` and its negation;
// `Glob::new(pat).is_err()` is a rejected pattern. A `Glob::new(pat)
// .unwrap()` with no match after it only says the pattern compiles and
// is counted, not harvested.
export function harvestWax(dir) {
  const file = "src/lib.rs";
  const src = readFileSync(join(dir, file), "utf8");
  const start = src.indexOf("#[cfg(test)]");
  const tests = start === -1 ? src : src.slice(start);
  const offset = start === -1 ? 0 : start;
  const cases = [];
  let acceptOnly = 0;
  let current = null;
  const re = /let glob = Glob::new\(|Glob::new\(|assert!\(\s*!?\s*glob\.is_match\(/g;
  for (const m of tests.matchAll(re)) {
    const abs = offset + m.index;
    const at = `${file}:${lineOf(src, abs)}`;
    const group = enclosingFn(src, abs);
    if (m[0].startsWith("assert!")) {
      let i = m.index + m[0].length;
      if (tests.startsWith("Path::new(", i)) i += "Path::new(".length;
      const path = literalAt(tests, i);
      if (path && current) cases.push(makeCase(current, path.value, {}, at, group));
      continue;
    }
    const pat = literalAt(tests, m.index + m[0].length);
    if (!pat) continue;
    const rest = tests.slice(pat.end, pat.end + 20);
    if (m[0].startsWith("let glob")) {
      current = pat.value;
    } else if (/^\)\s*\.is_err\(\)/.test(rest)) {
      cases.push(makeCase(pat.value, "-", {}, at, group, "reject"));
    } else if (/^\)\s*\.unwrap\(\)/.test(rest)) {
      acceptOnly++;
    }
  }
  return { cases, skipped: { acceptOnly }, failures: 0, files: 1 };
}
