#!/usr/bin/env node
// Corpus-driven correctness verification for both runtimes.
//
// Four corpora, all in `fixtures/corpus/`, share the same
// harness:
//
//   single — SINGLE_FILES list below   single-pattern × 2 lanes
//   multi  — `corpus-multi.txt`        N-pattern OR × 2 lanes
//   dir    — `corpus-dir.txt`          match_dir
//   mdir   — `corpus-multi-dir.txt`    N-pattern match_dir
//
// Two lanes per match row:
//   - globstar     — public API (`globstar(...)`, `Glob::new` / `Glob::union`)
//   - nfa          — the same matcher with the segment engine's anchored
//                    chains bypassed, so its NFA is checked on every row
//
// Nothing is skipped: a compile error where a row expects an answer is
// a failure (a single-pattern row may expect `err:Kind`), and a row the
// loader cannot read, or an exception that is not a GlobError, stops
// the run.
//
// Rust path runs `cargo test --test corpus -- --nocapture` and parses
// the test's `corpus=… engine=… pass=N fail=N` output. JS path is
// inlined below.
//
//   node scripts/verify.mjs              # full
//   node scripts/verify.mjs --skip-rust  # JS only
//   node scripts/verify.mjs --skip-js    # Rust only

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
process.chdir(ROOT);

const args = new Set(process.argv.slice(2));
const SKIP_RUST = args.has("--skip-rust");
const SKIP_JS = args.has("--skip-js");
for (const a of args) {
  if (a !== "--skip-rust" && a !== "--skip-js") {
    console.error(`unknown argument ${a}`);
    process.exit(2);
  }
}
if (SKIP_RUST && SKIP_JS) {
  console.error("--skip-rust and --skip-js together verify nothing");
  process.exit(2);
}

const FIXTURES_DIR = resolve(ROOT, "fixtures");

// The spec corpus, and the ecosystem corpus (`fixtures/ecosystem/`): the
// cases other libraries' suites assert, with our answers. The suites were
// run on POSIX, where the libraries themselves behave as recorded, so the
// ecosystem rows run everywhere but Windows.
const SINGLE_FILES = [
  "corpus/corpus.txt",
  "corpus/corpus-realworld.txt",
  "corpus/corpus-utf8.txt",
  "corpus/corpus-absolute.txt",
  "corpus/corpus-case.txt",
  "corpus/corpus-class.txt",
  "corpus/corpus-comprehensive.txt",
  "corpus/corpus-err.txt",
  process.platform === "win32" ? "corpus/corpus-windows.txt" : "corpus/corpus-unix.txt",
  ...(process.platform === "win32"
    ? []
    : [
        "ecosystem/js/picomatch.txt",
        "ecosystem/js/picomatch-diff.txt",
        "ecosystem/js/micromatch.txt",
        "ecosystem/js/micromatch-diff.txt",
        "ecosystem/js/minimatch.txt",
        "ecosystem/js/minimatch-diff.txt",
        "ecosystem/rust/globset.txt",
        "ecosystem/rust/globset-diff.txt",
        "ecosystem/rust/fast-glob.txt",
        "ecosystem/rust/fast-glob-diff.txt",
        "ecosystem/rust/wax.txt",
        "ecosystem/rust/wax-diff.txt",
      ]),
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

// The flags column: `dot=` and `case_insensitive=`, each `true` or
// `false`; anything else is a typo and stops the run.
function parseFlags(s, defaultDot, at) {
  const flags = { dot: defaultDot, caseInsensitive: false };
  for (const kv of s.split(",")) {
    const eq = kv.indexOf("=");
    if (eq < 0) throw new Error(`${at}: bad flag "${kv}"`);
    const k = kv.slice(0, eq).trim();
    const v = kv.slice(eq + 1).trim();
    if (v !== "true" && v !== "false") throw new Error(`${at}: bad flag value "${v}"`);
    if (k === "dot") flags.dot = v === "true";
    else if (k === "case_insensitive") flags.caseInsensitive = v === "true";
    else throw new Error(`${at}: unknown flag "${k}"`);
  }
  return flags;
}

function parseSummaryLines(text) {
  const out = [];
  for (const line of text.split("\n")) {
    const m = line.match(/^corpus=(\S+)\s+engine=(\S+)\s+pass=(\d+)\s+fail=(\d+)/);
    if (m) out.push({ corpus: m[1], engine: m[2], pass: +m[3], fail: +m[4] });
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

const makeStats = () => ({ pass: 0, fail: 0, failures: /** @type {string[]} */ ([]) });
// `why` is null when the row passed.
function record(stats, why, describe) {
  if (why === null) stats.pass++;
  else {
    stats.fail++;
    if (stats.failures.length < 10) stats.failures.push(`${describe()}: ${why}`);
  }
}

// The data rows of `file`: PATTERN, PATH, EXPECTED and the flags (with
// the default `dot`). A short row stops the run.
function rowsOf(file, defaultDot) {
  const rows = [];
  const lines = readFileSync(join(FIXTURES_DIR, file), "utf8").split("\n");
  lines.forEach((raw, i) => {
    const line = raw.replace(/\r$/, "");
    if (!line || line.startsWith("#")) return;
    const at = `${file}:${i + 1}`;
    const cols = line.split("\t");
    if (cols.length < 3) throw new Error(`${at}: a row needs PATTERN, PATH and EXPECTED`);
    const flags =
      cols.length >= 4
        ? parseFlags(cols[3], defaultDot, at)
        : { dot: defaultDot, caseInsensitive: false };
    rows.push({ at, raw: cols[0], path: unescape(cols[1]), expected: cols[2], ...flags });
  });
  return rows;
}

// The expected column: `{ err: kind }` for `err:Kind`, else the answer
// `value` reads.
function wantOf(row, value) {
  if (row.expected.startsWith("err:")) return { err: row.expected.slice(4) };
  const v = value(row.expected);
  if (v === undefined) throw new Error(`${row.at}: unknown expected "${row.expected}"`);
  return { value: v };
}

function patternsOf(row) {
  let patterns;
  try {
    patterns = JSON.parse(row.raw);
  } catch (e) {
    throw new Error(`${row.at}: bad PATTERNS_JSON ${row.raw} (${e.message})`);
  }
  if (!Array.isArray(patterns) || patterns.length === 0) {
    throw new Error(`${row.at}: PATTERNS_JSON must be a non-empty array`);
  }
  return patterns;
}

const describe = (row, pattern, engine) =>
  `${row.at}: pattern=${JSON.stringify(pattern)} path=${JSON.stringify(row.path)} dot=${row.dot} ci=${row.caseInsensitive}: ${engine}`;

const show = (want) => (want.err === undefined ? String(want.value) : `err:${want.err}`);

// ── JS-side runners ─────────────────────────────────────────────────

async function runJsVerify() {
  const { compileMatcher } = await import("../packages/globstar/src/glob.js");
  const { GlobError } = await import("../packages/globstar/src/error.js");

  // `{ value }`, or `{ err }` for a GlobError; anything else thrown is a
  // bug and stops the run.
  function attempt(f) {
    try {
      return { value: f() };
    } catch (e) {
      if (e instanceof GlobError) return { err: e.kind };
      throw e;
    }
  }
  function judge(got, want) {
    if (got.err === undefined && want.err === undefined && got.value === want.value) return null;
    if (got.err !== undefined && got.err === want.err) return null;
    return `got ${show(got)}, expected ${show(want)}`;
  }
  const opts = (row, engine) => ({
    dot: row.dot,
    caseInsensitive: row.caseInsensitive,
    __engine: engine,
  });
  const parseMatch = (s) => (s === "match" ? true : s === "no-match" ? false : undefined);

  // ── single-pattern and multi-pattern corpora, two lanes each
  const lanes = (rows, patternOf) => {
    const stats = { globstar: makeStats(), nfa: makeStats() };
    for (const row of rows) {
      const pattern = patternOf(row);
      const want = wantOf(row, parseMatch);
      for (const engine of ["globstar", "nfa"]) {
        const got = attempt(() =>
          compileMatcher(pattern, opts(row, engine === "nfa" ? "nfa" : undefined)).match(row.path),
        );
        record(stats[engine], judge(got, want), () => describe(row, pattern, engine));
      }
    }
    return stats;
  };
  const single = lanes(
    SINGLE_FILES.flatMap((f) => rowsOf(f, true)),
    (row) => unescape(row.raw),
  );
  const multi = lanes(rowsOf("corpus/corpus-multi.txt", true), patternsOf);

  // ── match_dir corpora — public API vs truth. Rows default to
  // `dot=false` (the walker convention); negated rows run too, a negated
  // pattern answers `Descend` or `DescendAndMatch` (§15.4). Indexed by JS `DirMatch` value
  // (Match=0, Pruned=1, Descend=2, DescendAndMatch=3, DescendAll=4,
  // DescendAllAndMatch=5).
  const DIR_TOKEN = [
    "match",
    "pruned",
    "descend",
    "descend-match",
    "descend-all",
    "descend-all-match",
  ];
  const parseDir = (s) => (DIR_TOKEN.includes(s) ? s : undefined);
  const dirLane = (rows, patternOf) => {
    const stats = makeStats();
    for (const row of rows) {
      const pattern = patternOf(row);
      const got = attempt(() => DIR_TOKEN[compileMatcher(pattern, opts(row)).matchDir(row.path)]);
      record(stats, judge(got, wantOf(row, parseDir)), () => describe(row, pattern, "globstar"));
    }
    return stats;
  };
  const dir = dirLane(rowsOf("corpus/corpus-dir.txt", false), (row) => unescape(row.raw));
  const multiDir = dirLane(rowsOf("corpus/corpus-multi-dir.txt", false), patternsOf);

  return [
    { corpus: "single", engine: "globstar", ...single.globstar },
    { corpus: "single", engine: "nfa", ...single.nfa },
    { corpus: "multi", engine: "globstar", ...multi.globstar },
    { corpus: "multi", engine: "nfa", ...multi.nfa },
    { corpus: "dir", engine: "globstar", ...dir },
    { corpus: "mdir", engine: "globstar", ...multiDir },
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
  `${pad("runtime", 8)} ${pad("corpus", 8)} ${pad("engine", 12)} ${rpad("pass", 6)} ${rpad("fail", 5)}`,
);
console.log("-".repeat(8 + 8 + 12 + 6 + 5 + 4));

// Stable cross-runtime row order: single → multi → dir → mdir,
// then globstar → nfa. Rust prints in cargo-test name
// order (alphabetical), JS in author order — sort so both line up.
const CORPUS_ORDER = { single: 0, multi: 1, dir: 2, mdir: 3 };
const ENGINE_ORDER = { globstar: 0, nfa: 1 };
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
      `${pad(side.runtime, 8)} ${pad(r.corpus, 8)} ${pad(r.engine, 12)} ${rpad(r.pass, 6)} ${rpad(r.fail, 5)}`,
    );
    totalFail += r.fail;
  }
  // A side that died, or printed no summary, is a failure however the
  // rows above look.
  if (side.spawnStatus !== 0 || side.rows.length === 0) {
    console.log(
      `${pad(side.runtime, 8)} ${pad("(missing)", 12)} (process exit ${side.spawnStatus})`,
    );
    totalFail += 1;
  }
}

// Both runtimes read the same files, so they must check the same number
// of rows: a row one loader reads and the other drops is a bug in one.
if (sides.length === 2) {
  const count = (side) =>
    new Map(side.rows.map((r) => [String(r.corpus) + "/" + String(r.engine), r.pass + r.fail]));
  const [rust, js] = sides.map(count);
  for (const [key, n] of rust) {
    if (js.get(key) !== n) {
      const other = js.has(key) ? String(js.get(key)) : "none";
      console.log(`row count differs for ${String(key)}: Rust ${String(n)}, JS ${other}`);
      totalFail += 1;
    }
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
