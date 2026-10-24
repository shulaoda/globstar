// The cases a Rust library's own tests assert, read from the test source:
// `matches!`/`nmatches!`/`syntaxerr!` in globset's glob.rs,
// `assert!(glob_match(..))` in fast-glob's test.rs, rstest rows in wax's
// lib.rs.

import { readFileSync } from "node:fs";
import { join } from "node:path";

// A Rust string literal at `src[i]` (`"…"`, `r"…"`, `r#"…"#`): its value
// and the index after it, or null.
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
    } else out += n;
  }
  return null;
}

const skipSpace = (src, i) => {
  while (i < src.length && /[\s,]/.test(src[i])) i++;
  return i;
};

function enclosingFn(src, i) {
  const before = src.slice(0, i);
  const m = [...before.matchAll(/fn\s+([a-z_0-9]+)\s*\(/g)].pop();
  return m ? m[1] : "";
}

const lineOf = (src, i) => src.slice(0, i).split("\n").length;

function makeCase(pattern, path, options, at, group, kind = "match") {
  return { pattern, path, options, at, group, kind };
}

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

// A `#[rstest]` block lists `#[case(…)]` rows, then a fn whose body says
// what they are: `assert_match_program_with(harness::assert_new_glob_is_ok("pat"),
// path, f)` matches one pattern against each row's path, or with
// `expression` in place of the literal each row is a (pattern, path) pair;
// `assert_new_glob_is_err(expression)` rows are rejected patterns;
// `assert_new_glob_is_ok(expression)` alone only says the pattern
// compiles and is counted, not harvested.
export function harvestWax(dir) {
  const file = "src/lib.rs";
  const src = readFileSync(join(dir, file), "utf8");
  const cases = [];
  let acceptOnly = 0;
  const blocks = /#\[rstest\]([\s\S]*?)\n    fn ([a-z_0-9]+)[\s\S]*?\{([\s\S]*?)\n    \}/g;
  for (const m of src.matchAll(blocks)) {
    const [, caseBlock, name, body] = m;
    const at = `${file}:${lineOf(src, m.index)}`;
    const rows = [];
    for (const c of caseBlock.matchAll(/#\[case\(\s*/g)) {
      const first = literalAt(caseBlock, c.index + c[0].length);
      if (!first) continue;
      const second = literalAt(caseBlock, skipSpace(caseBlock, first.end));
      rows.push([first.value, second ? second.value : null]);
    }
    const fixed = body.match(/assert_match_program_with\(\s*harness::assert_new_glob_is_ok\(/);
    if (fixed) {
      const i = fixed.index + fixed[0].length;
      const pat = literalAt(body, i);
      if (pat) {
        for (const [path] of rows) cases.push(makeCase(pat.value, path, {}, at, name));
      } else if (body.slice(i).startsWith("expression")) {
        for (const [pattern, path] of rows) {
          if (path !== null) cases.push(makeCase(pattern, path, {}, at, name));
        }
      }
    } else if (/assert_new_glob_is_err\(expression\)/.test(body)) {
      for (const [pattern] of rows) cases.push(makeCase(pattern, "-", {}, at, name, "reject"));
    } else if (/assert_new_glob_is_ok\(expression\)/.test(body)) {
      acceptOnly += rows.length;
    }
  }
  return { cases, skipped: { acceptOnly }, failures: 0, files: 1 };
}
