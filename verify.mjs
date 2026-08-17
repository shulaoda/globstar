#!/usr/bin/env node
// Corpus-driven correctness verification for both runtimes.
//
// Five corpora share the same harness:
//
//   single — SINGLE_FILES list below              single-pattern × 2 engines
//   multi  — `tests/corpus/corpus-multi.txt`      N-pattern OR × 2 engines
//   dir    — `tests/corpus/corpus-dir.txt`        match_dir × 2 engines
//   mdir   — `tests/corpus/corpus-multi-dir.txt`  N-pattern match_dir
//   err    — `tests/corpus/corpus-err.txt`        parse-error variants
//
// Two engines per match-corpus row:
//   - globstar     — public API (`globstar(...)`, `Glob::new` / `Glob::union`)
//   - PikeVm       — forced
//
// Rust path runs `cargo test --test corpus -- --nocapture` and parses
// the test's `corpus=… engine=… pass=N fail=N skip=N` output. JS path
// is inlined below.
//
//   node verify.mjs              # full
//   node verify.mjs --skip-rust  # JS only
//   node verify.mjs --skip-js    # Rust only

import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = dirname(fileURLToPath(import.meta.url));
process.chdir(ROOT);

const args = new Set(process.argv.slice(2));
const SKIP_RUST = args.has("--skip-rust");
const SKIP_JS = args.has("--skip-js");

const CORPUS_DIR = resolve(ROOT, "crates/globstar/tests/corpus");

const SINGLE_FILES = [
  "corpus.txt",
  "corpus-realworld.txt",
  "corpus-fast-glob.txt",
  "corpus-fast-glob-diff.txt",
  "corpus-utf8.txt",
  "corpus-absolute.txt",
  "corpus-case.txt",
  "corpus-class.txt",
  "corpus-comprehensive.txt",
  process.platform === "win32" ? "corpus-windows.txt" : "corpus-unix.txt",
];

// ── shared helpers ──────────────────────────────────────────────────

function unescape(s) {
  let out = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && i + 1 < s.length) {
      const n = s[i + 1];
      if (n === "\\") out += "\\";
      else if (n === "t") out += "\t";
      else if (n === "n") out += "\n";
      else out += "\\" + n;
      i++;
    } else {
      out += s[i];
    }
  }
  return out;
}

function parseFlags(s, defaultDot = true) {
  let dot = defaultDot;
  let caseInsensitive = false;
  for (const kv of s.split(",")) {
    const eq = kv.indexOf("=");
    if (eq < 0) continue;
    const k = kv.slice(0, eq).trim();
    const v = kv.slice(eq + 1).trim();
    if (k === "dot") dot = v === "true";
    else if (k === "case_insensitive") caseInsensitive = v === "true";
  }
  return { dot, caseInsensitive };
}

function parseSummaryLines(text) {
  const out = [];
  for (const line of text.split("\n")) {
    const m = line.match(/^corpus=(\S+)\s+engine=(\S+)\s+pass=(\d+)\s+fail=(\d+)\s+skip=(\d+)/);
    if (m) {
      out.push({
        corpus: m[1],
        engine: m[2],
        pass: +m[3],
        fail: +m[4],
        skip: +m[5],
      });
    }
  }
  return out;
}

function step(name, cmd, argv) {
  process.stderr.write(`\n[verify] ${name} → ${cmd} ${argv.join(" ")}\n`);
  const t0 = Date.now();
  const res = spawnSync(cmd, argv, {
    stdio: ["inherit", "pipe", "pipe"],
    encoding: "utf-8",
  });
  const dt = ((Date.now() - t0) / 1000).toFixed(1);
  if (res.status === 0) {
    process.stderr.write(`[verify]   ok in ${dt}s\n`);
  } else {
    process.stderr.write(`[verify]   exit=${res.status} in ${dt}s\n`);
  }
  return { status: res.status, stdout: res.stdout || "", stderr: res.stderr || "" };
}

const makeStats = () => ({ pass: 0, fail: 0, skip: 0, failures: [] });
function record(stats, ok, msgFn) {
  if (ok) stats.pass++;
  else {
    stats.fail++;
    if (stats.failures.length < 10) stats.failures.push(msgFn());
  }
}

// ── JS-side runners ─────────────────────────────────────────────────

async function runJsVerify() {
  const { globstar } = await import("./packages/globstar/src/index.js");
  const { compileMatcher } = await import("./packages/globstar/src/glob.js");
  const { GlobError } = await import("./packages/globstar/src/error.js");

  // ── single-pattern corpus
  const filenames = new Set(readdirSync(CORPUS_DIR));
  function* singleRows() {
    for (const f of SINGLE_FILES) {
      if (!filenames.has(f)) continue;
      const text = readFileSync(join(CORPUS_DIR, f), "utf8");
      let lineNo = 0;
      for (const raw of text.split("\n")) {
        lineNo++;
        const line = raw.replace(/\s+$/, "");
        if (!line || line.startsWith("#")) continue;
        const cols = line.split("\t");
        if (cols.length < 3) continue;
        const exp = cols[2];
        if (exp !== "match" && exp !== "no-match") continue;
        const flags =
          cols.length >= 4 ? parseFlags(cols[3]) : { dot: true, caseInsensitive: false };
        yield {
          file: f,
          lineNo,
          pattern: unescape(cols[0]),
          path: unescape(cols[1]),
          expected: exp === "match",
          ...flags,
        };
      }
    }
  }

  function runSinglePub(row) {
    try {
      return globstar(row.pattern, { dot: row.dot, caseInsensitive: row.caseInsensitive })(
        row.path,
      );
    } catch {
      return null;
    }
  }
  function runSingleEngine(row, engineName) {
    try {
      return compileMatcher(row.pattern, {
        dot: row.dot,
        caseInsensitive: row.caseInsensitive,
        __engine: engineName,
      }).match(row.path);
    } catch {
      return null;
    }
  }
  const singleFail = (row, engine, got) =>
    `${row.file}:${row.lineNo}: pattern=${JSON.stringify(row.pattern)} path=${JSON.stringify(row.path)} dot=${row.dot} ci=${row.caseInsensitive}: ${engine} got ${got}, expected ${row.expected}`;

  const single = {
    globstar: makeStats(),
    PikeVm: makeStats(),
  };
  for (const row of singleRows()) {
    const g = runSinglePub(row);
    if (g === null) single.globstar.skip++;
    else record(single.globstar, g === row.expected, () => singleFail(row, "globstar", g));

    const p = runSingleEngine(row, "pikevm");
    if (p === null) single.PikeVm.skip++;
    else record(single.PikeVm, p === row.expected, () => singleFail(row, "PikeVm", p));
  }

  // ── multi-pattern corpus
  function* multiRows() {
    const path = join(CORPUS_DIR, "corpus-multi.txt");
    const text = readFileSync(path, "utf8");
    let lineNo = 0;
    for (const raw of text.split("\n")) {
      lineNo++;
      const line = raw.replace(/\s+$/, "");
      if (!line || line.startsWith("#")) continue;
      const cols = line.split("\t");
      if (cols.length < 3) continue;
      let patterns;
      try {
        patterns = JSON.parse(cols[0]);
      } catch (e) {
        throw new Error(`corpus-multi.txt:${lineNo}: bad PATTERNS_JSON ${cols[0]} (${e.message})`);
      }
      if (!Array.isArray(patterns) || patterns.length === 0) {
        throw new Error(`corpus-multi.txt:${lineNo}: PATTERNS_JSON must be non-empty array`);
      }
      const exp = cols[2];
      if (exp !== "match" && exp !== "no-match") {
        throw new Error(`corpus-multi.txt:${lineNo}: unknown expected "${exp}"`);
      }
      const flags = cols.length >= 4 ? parseFlags(cols[3]) : { dot: true, caseInsensitive: false };
      yield {
        lineNo,
        patterns,
        path: unescape(cols[1]),
        expected: exp === "match",
        ...flags,
      };
    }
  }

  function runMultiPub(row) {
    try {
      return globstar(row.patterns, { dot: row.dot, caseInsensitive: row.caseInsensitive })(
        row.path,
      );
    } catch {
      return null;
    }
  }
  function runMultiEngine(row, engineName) {
    try {
      return compileMatcher(row.patterns, {
        dot: row.dot,
        caseInsensitive: row.caseInsensitive,
        __engine: engineName,
      }).match(row.path);
    } catch {
      return null;
    }
  }
  const multiFail = (row, engine, got) =>
    `corpus-multi.txt:${row.lineNo}: patterns=${JSON.stringify(row.patterns)} path=${JSON.stringify(row.path)} dot=${row.dot} ci=${row.caseInsensitive}: ${engine} got ${got}, expected ${row.expected}`;

  const multi = {
    globstar: makeStats(),
    PikeVm: makeStats(),
  };
  for (const row of multiRows()) {
    const g = runMultiPub(row);
    if (g === null) multi.globstar.skip++;
    else record(multi.globstar, g === row.expected, () => multiFail(row, "globstar", g));

    const p = runMultiEngine(row, "pikevm");
    if (p === null) multi.PikeVm.skip++;
    else record(multi.PikeVm, p === row.expected, () => multiFail(row, "PikeVm", p));
  }

  // ── match_dir corpus — public SSM plus reference engines vs truth.
  // Mirrors the Rust `corpus_dir_engines_vs_truth` test: rows default to
  // `dot=false` (the walker convention), and negated patterns are skipped
  // because `match_dir` does not invert a leading `!`.
  // Indexed by JS `DirMatch` value (Match=0, Pruned=1, Descend=2, DescendAndMatch=3).
  const DIR_TOKEN = ["match", "pruned", "descend", "descend-match"];
  function* dirRows() {
    const text = readFileSync(join(CORPUS_DIR, "corpus-dir.txt"), "utf8");
    let lineNo = 0;
    for (const raw of text.split("\n")) {
      lineNo++;
      const line = raw.replace(/\s+$/, "");
      if (!line || line.startsWith("#")) continue;
      const cols = line.split("\t");
      if (cols.length < 3) continue;
      const expected = cols[2];
      if (!DIR_TOKEN.includes(expected)) continue;
      const flags =
        cols.length >= 4 ? parseFlags(cols[3], false) : { dot: false, caseInsensitive: false };
      yield {
        lineNo,
        pattern: unescape(cols[0]),
        path: unescape(cols[1]),
        expected,
        ...flags,
      };
    }
  }

  function runDirEngine(row, engineName) {
    // Both variants go through `compileMatcher`, whose `matchDir` inverts
    // leading-`!` negation at the wrapper level (conservative Descend,
    // §13.4) regardless of which engine `__engine` forces the body onto —
    // so negated rows are exercised on BOTH engines and no row is skipped.
    try {
      const dm = compileMatcher(row.pattern, {
        dot: row.dot,
        caseInsensitive: row.caseInsensitive,
        __engine: engineName,
      }).matchDir(row.path);
      return DIR_TOKEN[dm];
    } catch {
      return null;
    }
  }
  const dirFail = (row, engine, got) =>
    `corpus-dir.txt:${row.lineNo}: pattern=${JSON.stringify(row.pattern)} dir=${JSON.stringify(row.path)} dot=${row.dot} ci=${row.caseInsensitive}: ${engine} got ${got}, expected ${row.expected}`;

  const dir = {
    globstar: makeStats(),
    PikeVm: makeStats(),
  };
  for (const row of dirRows()) {
    const g = runDirEngine(row, undefined); // public default engine (Segment/PikeVm)
    if (g === null) dir.globstar.skip++;
    else record(dir.globstar, g === row.expected, () => dirFail(row, "globstar", g));

    const p = runDirEngine(row, "pikevm");
    if (p === null) dir.PikeVm.skip++;
    else record(dir.PikeVm, p === row.expected, () => dirFail(row, "PikeVm", p));
  }

  // ── multi-pattern match_dir corpus — union built the same way as the
  // multi corpus, judged with the dir tokens. Rows default to dot=false
  // (walker convention), mirroring `corpus_multi_dir_engines_vs_truth`.
  function* multiDirRows() {
    const text = readFileSync(join(CORPUS_DIR, "corpus-multi-dir.txt"), "utf8");
    let lineNo = 0;
    for (const raw of text.split("\n")) {
      lineNo++;
      const line = raw.replace(/\s+$/, "");
      if (!line || line.startsWith("#")) continue;
      const cols = line.split("\t");
      if (cols.length < 3) continue;
      let patterns;
      try {
        patterns = JSON.parse(cols[0]);
      } catch (e) {
        throw new Error(
          `corpus-multi-dir.txt:${lineNo}: bad PATTERNS_JSON ${cols[0]} (${e.message})`,
        );
      }
      const expected = cols[2];
      if (!DIR_TOKEN.includes(expected)) {
        throw new Error(`corpus-multi-dir.txt:${lineNo}: unknown DirMatch "${expected}"`);
      }
      const flags =
        cols.length >= 4 ? parseFlags(cols[3], false) : { dot: false, caseInsensitive: false };
      yield {
        lineNo,
        patterns,
        path: unescape(cols[1]),
        expected,
        ...flags,
      };
    }
  }

  function runMultiDirEngine(row, engineName) {
    try {
      const dm = compileMatcher(row.patterns, {
        dot: row.dot,
        caseInsensitive: row.caseInsensitive,
        __engine: engineName,
      }).matchDir(row.path);
      return DIR_TOKEN[dm];
    } catch {
      return null;
    }
  }
  const multiDirFail = (row, engine, got) =>
    `corpus-multi-dir.txt:${row.lineNo}: patterns=${JSON.stringify(row.patterns)} dir=${JSON.stringify(row.path)} dot=${row.dot} ci=${row.caseInsensitive}: ${engine} got ${got}, expected ${row.expected}`;

  const multiDir = {
    globstar: makeStats(),
    PikeVm: makeStats(),
  };
  for (const row of multiDirRows()) {
    const g = runMultiDirEngine(row, undefined);
    if (g === null) multiDir.globstar.skip++;
    else record(multiDir.globstar, g === row.expected, () => multiDirFail(row, "globstar", g));

    const p = runMultiDirEngine(row, "pikevm");
    if (p === null) multiDir.PikeVm.skip++;
    else record(multiDir.PikeVm, p === row.expected, () => multiDirFail(row, "PikeVm", p));
  }

  // ── parse-error corpus — public API only (engines never see malformed input)
  const errStats = makeStats();
  {
    const path = join(CORPUS_DIR, "corpus-err.txt");
    const text = readFileSync(path, "utf8");
    let lineNo = 0;
    for (const raw of text.split("\n")) {
      lineNo++;
      const line = raw.replace(/\s+$/, "");
      if (!line || line.startsWith("#")) continue;
      const cols = line.split("\t");
      if (cols.length < 2) continue;
      const pattern = unescape(cols[0]);
      const expectedKind = cols[1];

      let kind = null;
      try {
        compileMatcher(pattern);
      } catch (e) {
        kind = e instanceof GlobError ? e.kind : null;
      }
      record(
        errStats,
        kind === expectedKind,
        () =>
          `corpus-err.txt:${lineNo}: pattern=${JSON.stringify(pattern)}: expected ${expectedKind}, got ${kind === null ? "(no error)" : kind}`,
      );
    }
  }

  return [
    { corpus: "single", engine: "globstar", ...single.globstar },
    { corpus: "single", engine: "PikeVm", ...single.PikeVm },
    { corpus: "multi", engine: "globstar", ...multi.globstar },
    { corpus: "multi", engine: "PikeVm", ...multi.PikeVm },
    { corpus: "dir", engine: "globstar", ...dir.globstar },
    { corpus: "dir", engine: "PikeVm", ...dir.PikeVm },
    { corpus: "mdir", engine: "globstar", ...multiDir.globstar },
    { corpus: "mdir", engine: "PikeVm", ...multiDir.PikeVm },
    { corpus: "err", engine: "globstar", ...errStats },
  ];
}

// ── orchestration ───────────────────────────────────────────────────

const sides = [];

if (!SKIP_RUST) {
  const r = step("rust corpus", "cargo", ["test", "--test", "corpus", "--", "--nocapture"]);
  sides.push({
    runtime: "Rust",
    rows: parseSummaryLines(r.stdout + r.stderr),
    spawnStatus: r.status,
    rawStderr: r.stderr,
  });
}

if (!SKIP_JS) {
  process.stderr.write(`\n[verify] js corpus (inline)\n`);
  const t0 = Date.now();
  const rows = await runJsVerify();
  const dt = ((Date.now() - t0) / 1000).toFixed(1);
  process.stderr.write(`[verify]   done in ${dt}s\n`);
  // Rows carry up to 10 failure samples each (makeStats); join them so
  // the failure-samples block below prints JS diagnostics too.
  const jsFailures = rows.flatMap((r) => r.failures.map((m) => `[${r.corpus}/${r.engine}] ${m}`));
  sides.push({ runtime: "JS", rows, spawnStatus: 0, rawStderr: jsFailures.join("\n") });
}

// ── unified summary ─────────────────────────────────────────────────

const pad = (s, n) => {
  s = String(s);
  return s + " ".repeat(Math.max(0, n - s.length));
};
const rpad = (s, n) => {
  s = String(s);
  return " ".repeat(Math.max(0, n - s.length)) + s;
};

console.log("\n=== verify-corpus summary ===");
console.log(
  `${pad("runtime", 8)} ${pad("corpus", 8)} ${pad("engine", 12)} ${rpad("pass", 6)} ${rpad("fail", 5)} ${rpad("skip", 5)}`,
);
console.log("-".repeat(8 + 8 + 12 + 6 + 5 + 5 + 5));

// Stable cross-runtime row order: single → multi → err, then
// globstar → PikeVm. Rust prints in cargo-test name
// order (alphabetical), JS in author order — sort so both line up.
const CORPUS_ORDER = { single: 0, multi: 1, dir: 2, mdir: 3, err: 4 };
const ENGINE_ORDER = { globstar: 0, PikeVm: 1 };
const rowKey = (r) => [CORPUS_ORDER[r.corpus] ?? 99, ENGINE_ORDER[r.engine] ?? 99];
for (const side of sides) {
  side.rows.sort((a, b) => {
    const ka = rowKey(a);
    const kb = rowKey(b);
    return ka[0] - kb[0] || ka[1] - kb[1];
  });
}

let totalFail = 0;
for (const side of sides) {
  for (const r of side.rows) {
    console.log(
      `${pad(side.runtime, 8)} ${pad(r.corpus, 8)} ${pad(r.engine, 12)} ${rpad(r.pass, 6)} ${rpad(r.fail, 5)} ${rpad(r.skip, 5)}`,
    );
    totalFail += r.fail;
  }
  if (side.rows.length === 0) {
    console.log(
      `${pad(side.runtime, 8)} ${pad("(missing)", 12)} (process exit ${side.spawnStatus})`,
    );
    totalFail += 1;
  }
}

if (totalFail > 0) {
  console.log("\n--- failure samples (first 10 per engine, per side) ---");
  for (const side of sides) {
    if (side.rawStderr.trim()) {
      console.log(`\n[${side.runtime}]`);
      console.log(side.rawStderr.trimEnd());
    }
  }
  process.exit(1);
}

console.log("\n✓ all engines on all corpora agree with expected truth");
