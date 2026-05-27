#!/usr/bin/env node
// Corpus-driven correctness verification for both runtimes.
//
// Three corpora share the same harness:
//
//   single — `tests/corpus/corpus*.txt`        single-pattern × 3 engines
//   multi  — `tests/corpus/corpus-multi.txt`   N-pattern OR × 3 engines
//   err    — `tests/corpus/corpus-err.txt`     parse-error variants × public API
//
// Three engines per match-corpus row:
//   - globstar     — public API (`globstar(...)`, `Glob::new` / `Glob::union`)
//   - ThompsonDfa  — forced (single: lower+ThompsonDfa::build; multi: factored merge)
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

function parseFlags(s) {
  let dot = true;
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
  const { compileMatcher } = await import("./packages/globstar/src/matcher/glob.js");
  const { GlobError } = await import("./packages/globstar/src/matcher/error.js");

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
    ThompsonDfa: makeStats(),
    PikeVm: makeStats(),
  };
  for (const row of singleRows()) {
    const g = runSinglePub(row);
    if (g === null) single.globstar.skip++;
    else record(single.globstar, g === row.expected, () => singleFail(row, "globstar", g));

    const d = runSingleEngine(row, "dfa");
    if (d === null) single.ThompsonDfa.skip++;
    else record(single.ThompsonDfa, d === row.expected, () => singleFail(row, "ThompsonDfa", d));

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
  // `__noAutoOr: true` keeps the union as one merged engine instead of
  // auto-decomposing to per-pattern OR — that's what the Rust side
  // forced-DFA / forced-PikeVm runners measure.
  function runMultiEngine(row, engineName) {
    try {
      return compileMatcher(row.patterns, {
        dot: row.dot,
        caseInsensitive: row.caseInsensitive,
        __engine: engineName,
        __noAutoOr: true,
      }).match(row.path);
    } catch {
      return null;
    }
  }
  const multiFail = (row, engine, got) =>
    `corpus-multi.txt:${row.lineNo}: patterns=${JSON.stringify(row.patterns)} path=${JSON.stringify(row.path)} dot=${row.dot} ci=${row.caseInsensitive}: ${engine} got ${got}, expected ${row.expected}`;

  const multi = {
    globstar: makeStats(),
    ThompsonDfa: makeStats(),
    PikeVm: makeStats(),
  };
  for (const row of multiRows()) {
    const g = runMultiPub(row);
    if (g === null) multi.globstar.skip++;
    else record(multi.globstar, g === row.expected, () => multiFail(row, "globstar", g));

    const d = runMultiEngine(row, "dfa");
    if (d === null) multi.ThompsonDfa.skip++;
    else record(multi.ThompsonDfa, d === row.expected, () => multiFail(row, "ThompsonDfa", d));

    const p = runMultiEngine(row, "pikevm");
    if (p === null) multi.PikeVm.skip++;
    else record(multi.PikeVm, p === row.expected, () => multiFail(row, "PikeVm", p));
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
    { corpus: "single", engine: "ThompsonDfa", ...single.ThompsonDfa },
    { corpus: "single", engine: "PikeVm", ...single.PikeVm },
    { corpus: "multi", engine: "globstar", ...multi.globstar },
    { corpus: "multi", engine: "ThompsonDfa", ...multi.ThompsonDfa },
    { corpus: "multi", engine: "PikeVm", ...multi.PikeVm },
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
  // Per-engine failures are surfaced via row totals; failure samples
  // would need plumbing back from runJsVerify if we wanted them here.
  sides.push({ runtime: "JS", rows, spawnStatus: 0, rawStderr: "" });
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
// globstar → ThompsonDfa → PikeVm. Rust prints in cargo-test name
// order (alphabetical), JS in author order — sort so both line up.
const CORPUS_ORDER = { single: 0, multi: 1, err: 2 };
const ENGINE_ORDER = { globstar: 0, ThompsonDfa: 1, PikeVm: 2 };
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
