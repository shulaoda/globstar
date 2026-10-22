#!/usr/bin/env node
// One-click benchmark runner.
//
// Compares globstar (default public API) against the dominant
// ecosystem libraries on three axes (compile / match / memory) for
// both single-pattern and multi-pattern matchers, plus end-to-end
// walker timings. Rust + JS columns are merged into single tables so
// rows can be read across runtimes.
//
//   node scripts/bench.mjs              # full run (~5 min on Apple Silicon)
//   node scripts/bench.mjs --skip-rust  # JS only
//   node scripts/bench.mjs --skip-js    # Rust only

import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { PATHS, PATTERNS, PATTERN_SETS } from "../packages/bench/benches/shared.js";

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
process.chdir(ROOT);

const args = new Set(process.argv.slice(2));
const SKIP_RUST = args.has("--skip-rust");
const SKIP_JS = args.has("--skip-js");

// The matcher row labels, from the fixtures both runtimes' benches read.
const SINGLE_PATTERNS = PATTERNS.map(([label]) => label);
const MULTI_SETS = PATTERN_SETS.map(([label]) => label);

// Walker row labels — `<scale>_<pattern-set>`.
const WALKER_LABELS = [
  "small_multi-small",
  "small_multi-huge",
  "medium_multi-small",
  "medium_multi-huge",
  "large_multi-small",
  "large_multi-huge",
];

// ── helpers ──────────────────────────────────────────────────────

let failed = false;

// The Rust side is built with every function on a 64-byte boundary and
// every jump target on a 32-byte one, in its own target directory.
// Without them, code placement alone moved a 2 ns row 1.7× and a
// 190 ns row 1.15× between builds that ran the same instructions.
const RUST_ENV = {
  ...process.env,
  RUSTFLAGS: "-C llvm-args=-align-all-functions=6 -C llvm-args=-align-all-nofallthru-blocks=5",
  CARGO_TARGET_DIR: "target/bench",
};

function step(name, cmd, argv) {
  process.stderr.write(`\n[bench] ${name} → ${cmd} ${argv.join(" ")}\n`);
  const t0 = Date.now();
  const res = spawnSync(cmd, argv, {
    stdio: ["inherit", "pipe", "pipe"],
    encoding: "utf-8",
    env: cmd === "cargo" ? RUST_ENV : process.env,
  });
  const dt = ((Date.now() - t0) / 1000).toFixed(1);
  if (res.status !== 0) {
    failed = true;
    process.stderr.write(`[bench]   FAILED in ${dt}s (exit ${res.status})\n`);
    process.stderr.write((res.stderr || "").slice(0, 2000));
  } else {
    process.stderr.write(`[bench]   ok in ${dt}s\n`);
  }
  return (res.stdout || "") + (res.stderr || "");
}

// ANSI escape sequences are inherently control-char-prefixed.
// eslint-disable-next-line no-control-regex
const stripAnsi = (s) => s.replace(/\u001b\[[0-9;]*m/g, "");

// ── parsers ──────────────────────────────────────────────────────

function parseCriterion(text) {
  const out = {};
  const UNIT = "(?:ns|µs|ms|s)";
  const re = new RegExp(
    `^([\\w\\-]+)/([\\w\\-().]+)\\s+time:\\s+` +
      `\\[[\\d.]+\\s${UNIT}\\s([\\d.]+)\\s(${UNIT})\\s[\\d.]+\\s${UNIT}\\]`,
    "gm",
  );
  let m;
  while ((m = re.exec(text))) {
    const [, group, bench, mid, unit] = m;
    (out[group] ??= {})[bench] = `${mid} ${unit}`;
  }
  return out;
}

function parseMitata(text) {
  const out = {};
  let group = null;
  for (const line of stripAnsi(text).split("\n")) {
    const g = line.match(/^•\s+(.+?)\s*$/);
    if (g) {
      group = g[1];
      out[group] = {};
      continue;
    }
    const b = line.match(/^([A-Za-z][\w\-_().\s]*?)\s+([\d.]+)\s*(ns|µs|ms)\/iter/);
    if (b && group) {
      const [, name, v, u] = b;
      out[group][name.trim()] = `${v} ${u}`;
    }
  }
  return out;
}

function parseTable(text, headerHint) {
  const split = (line) =>
    (line.includes("|") ? line.split(/\s*\|\s*/) : line.split(/\s{2,}/))
      .map((c) => c.trim())
      .filter(Boolean);
  const lines = text.split("\n").map((l) => l.trimEnd());
  const out = {};
  let cols = null;
  for (const line of lines) {
    if (!line.trim()) continue;
    if (line.includes(headerHint) && cols === null) {
      cols = split(line);
      continue;
    }
    if (!cols) continue;
    if (line.match(/^[-\s|]+$/)) continue;
    const cells = split(line);
    if (cells.length !== cols.length) continue;
    const row = {};
    for (let i = 1; i < cols.length; i++) row[cols[i]] = cells[i];
    out[cells[0]] = row;
  }
  return out;
}

// ── formatting ──────────────────────────────────────────────────

const UNIT_FACTOR = { ns: 1, µs: 1000, ms: 1_000_000, s: 1_000_000_000 };
const UNIT_ORDER = ["ns", "µs", "ms", "s"];

function timeToNs(s) {
  const m = s.match(/^([\d.]+)\s*(ns|µs|ms|s)$/);
  if (!m) return null;
  return parseFloat(m[1]) * UNIT_FACTOR[m[2]];
}

function nsToString(ns) {
  // Pick the unit that puts the value in [1, 1000).
  for (const u of UNIT_ORDER) {
    const factor = UNIT_FACTOR[u];
    if (ns < factor * 1000 || u === "s") {
      return `${(ns / factor).toFixed(2)} ${u}`;
    }
  }
}

function formatCell(val, opts = {}) {
  if (!val || val === "—") return val;
  const ns = timeToNs(val);
  if (ns !== null) {
    const scaled = opts.divideBy ? ns / opts.divideBy : ns;
    return nsToString(scaled);
  }
  // Bare integer (Rust memory tools).
  if (/^\d+$/.test(val) && opts.isMem) return bytesToKb(parseInt(val, 10));
  // JS memory: `424 B` style.
  const m = val.match(/^([\d.]+)\s*B$/);
  if (m) return bytesToKb(parseFloat(m[1]));
  return val;
}

function bytesToKb(bytes) {
  return `${(bytes / 1024).toFixed(2)} KB`;
}

// ── runners ──────────────────────────────────────────────────────

const rust = {};
const js = {};

if (!SKIP_RUST) {
  rust.matcherSingle = parseCriterion(
    step("rust matcher single", "cargo", ["bench", "--bench", "matcher_single", "--", "--quick"]),
  );
  rust.matcherMulti = parseCriterion(
    step("rust matcher multi", "cargo", ["bench", "--bench", "matcher_multi", "--", "--quick"]),
  );
  rust.walker = parseCriterion(
    step("rust walker", "cargo", [
      "bench",
      "-p",
      "globstar-walk",
      "--bench",
      "walker",
      "--",
      "--quick",
    ]),
  );
  rust.singleMem = parseTable(
    // Through cargo, so a CARGO_TARGET_DIR build is the one that runs.
    step("rust single memory", "cargo", [
      "run",
      "--release",
      "--quiet",
      "-p",
      "memory-check",
      "--bin",
      "matcher-single-memory",
    ]),
    "Pattern",
  );
  rust.multiMem = parseTable(
    step("rust multi memory", "cargo", [
      "run",
      "--release",
      "--quiet",
      "-p",
      "memory-check",
      "--bin",
      "matcher-multi-memory",
    ]),
    "Pattern set",
  );
}

if (!SKIP_JS) {
  const jsSingleRaw = step("js matcher single", "node", [
    "--expose-gc",
    "packages/bench/benches/matcher_single.js",
  ]);
  js.matcherSingle = parseMitata(jsSingleRaw);
  js.singleMem = parseTable(jsSingleRaw, "Pattern  ");

  const jsMultiRaw = step("js matcher multi", "node", [
    "--expose-gc",
    "packages/bench/benches/matcher_multi.js",
  ]);
  js.matcherMulti = parseMitata(jsMultiRaw);
  js.multiMem = parseTable(jsMultiRaw, "Pattern set");

  js.walker = parseMitata(
    step("js walker", "node", ["--expose-gc", "packages/bench/benches/walker.js"]),
  );
}

// ── markdown emission ────────────────────────────────────────────

function table(headers, rows) {
  if (rows.length === 0) return "_(no data)_";
  const sep = headers.map(() => "---").join(" | ");
  const head = headers.join(" | ");
  const body = rows.map((r) => `| ${r.join(" | ")} |`).join("\n");
  return `| ${head} |\n| ${sep} |\n${body}`;
}

/**
 * Emit one table that pulls cells from multiple sources (typically the
 * Rust and JS sides) and joins them on a shared row-label list.
 *
 * `sources[i]` is `{ data, rowKey, libCols, cellOpts }` where
 *   data        — parsed `{ group: { libKey: "v unit" } }` map
 *   rowKey(lbl) — translates a label like `"literal"` into the parser's
 *                 group key (`"compile_literal"` for criterion,
 *                 `"compile: literal"` for mitata, etc.)
 *   libCols     — `[[displayName, lookupKey], …]`
 *   cellOpts    — `{ isMem, divideBy }` passed through to `formatCell`
 */
const missingCells = [];

function combinedTable(rowLabelHeader, rowLabels, sources) {
  const headers = [rowLabelHeader, ...sources.flatMap((s) => s.libCols.map((c) => c[0]))];
  const rows = rowLabels.map((label) => {
    const cells = sources.flatMap((src) => {
      const groupKey = src.rowKey(label);
      const group = src.data[groupKey] ?? {};
      return src.libCols.map(([name, key]) => {
        // A library that threw in mitata leaves no number, not a failed
        // step: count the hole so the run fails rather than print "—".
        if (group[key] === undefined) missingCells.push(`${label} / ${name}`);
        return formatCell(group[key] ?? "—", src.cellOpts ?? {});
      });
    });
    return [label, ...cells];
  });
  return table(headers, rows);
}

// ── source helpers ──────────────────────────────────────────────

const empty = {};

// The four matcher tables of one runtime: `data` is its criterion or
// mitata parse, `mem` its memory table, `key(phase, label)` the group
// key of a row, `cols` the library columns (`match` adds fast_glob on
// the Rust side). Rust criterion times the whole path batch per iter;
// `divideBy` brings it onto the per-call basis mitata reports.
function matcherTables(data, mem, key, cols, { match = cols, divideBy } = {}) {
  const src = (d, rowKey, libCols, cellOpts) => ({ data: d ?? empty, rowKey, libCols, cellOpts });
  return {
    build: src(data, (l) => key("build", l), cols),
    compile: src(data, (l) => key("compile", l), cols),
    match: src(data, (l) => key("match", l), match, divideBy && { divideBy }),
    mem: src(mem, (l) => l, cols, { isMem: true }),
  };
}
const libCols = (runtime, names) => names.map((n) => [`${n} (${runtime})`, n]);
const RUST_COLS = libCols("Rust", ["globstar", "globset", "wax"]);
const RUST_MATCH_COLS = libCols("Rust", ["globstar", "globset", "wax", "fast_glob"]);
const JS_COLS = libCols("JS", ["globstar", "picomatch", "minimatch", "micromatch"]);
const rustKey = (phase, l) => `${phase}_${l}`;
const jsKey = (phase, l) => `${phase}: ${l}`;
const rustOpts = { match: RUST_MATCH_COLS, divideBy: PATHS.length };

const rustSingle = matcherTables(rust.matcherSingle, rust.singleMem, rustKey, RUST_COLS, rustOpts);
const jsSingle = matcherTables(js.matcherSingle, js.singleMem, jsKey, JS_COLS);
const rustMulti = matcherTables(rust.matcherMulti, rust.multiMem, rustKey, RUST_COLS, rustOpts);
const jsMulti = matcherTables(js.matcherMulti, js.multiMem, jsKey, JS_COLS);

const rustWalker = {
  data: rust.walker ?? empty,
  rowKey: (l) => `walk_${l}`,
  libCols: [
    ["globstar-walk (Rust)", "globstar_walk"],
    ["globwalk (Rust)", "globwalk"],
    ["ignore (Rust)", "ignore"],
  ],
};

const jsWalker = {
  data: js.walker ?? empty,
  rowKey: (l) => `walk_${l}`,
  libCols: [
    ["globstar (JS)", "globstar"],
    ["fast-glob (JS)", "fast-glob"],
    ["globby (JS)", "globby"],
    ["tinyglobby (JS)", "tinyglobby"],
  ],
};

// ── report assembly ─────────────────────────────────────────────

const sections = [];

sections.push(`# Benchmarks

Generated by \`node scripts/bench.mjs\` on ${new Date().toISOString().slice(0, 19)} at ${
  spawnSync("git", ["rev-parse", "--short", "HEAD"], { encoding: "utf8" }).stdout?.trim() ?? "?"
}.

Build, build + first non-match, match and memory tables for both single-
and multi-pattern matchers, plus end-to-end walker timings. Rust and JS
columns share each table — every row exercises the **same pattern and
match input on both runtimes**. Numbers are means: criterion's
estimate from a \`--quick\` run (Rust) and mitata's \`avg\` (JS). No
spread is shown, so a gap of a few percent between two runs is noise;
use them for relative comparison, not absolute SLOs. The Rust binaries
are built with every function 64-byte aligned and every jump target
32-byte aligned, so code placement does not move the shortest rows
between builds.

> **Same answers.** Every library runs with \`dot: true\` (globstar's
> default), and each bench checks before timing that all of them give
> the same answer on every row.

> **Build cells** time building a matcher for the pattern, and nothing
> else: what a library does before its first match. On the JS side each
> iteration gets a pattern not seen before, of the row's shape (a short
> literal tag added), since V8 caches a RegExp by its source.

> **Build + first non-match cells** time that build plus the matcher's
> first call: the regex-based libraries compile their regex (V8 does it
> on first use) or build their search cache (globset, wax) only then, so
> this is what a one-shot use pays. The call is on a path no row
> matches, so a list matched one member at a time runs every member
> instead of stopping at a hit.

> **Memory cells** count what a matcher keeps once it has matched
> every bench path: JS from up to 1,000 distinct patterns of the row's
> shape per trial (fewer for a wide union, so the regex libraries'
> RegExps fit V8's code space); Rust from the allocator's count around
> the build and those matches.

> **Match cell semantics.** Rust criterion times the full ${PATHS.length}-path
> batch per iter; JS mitata times one match call per iter. The
> orchestrator divides the Rust value by ${PATHS.length} so all \`match\` columns
> show the same per-call basis.
`);

sections.push(`## Test corpus

### Single-pattern test cases

| Row | Pattern | What it tests |
|---|---|---|
| \`literal\` | \`src/main.rs\` | pure byte-equality, no metacharacters |
| \`simple-wildcard\` | \`src/*.ts\` | one segment-local \`*\` |
| \`globstar\` | \`src/**/*.ts\` | \`**\` + segment wildcard |
| \`brace-suffix\` | \`**/*.{ts,tsx,js,jsx}\` | \`**\` + 4-way brace alternation |
| \`reject-prefilter\` | \`**/*.md\` | \`**\` + suffix-anchored prefilter (rejects most paths) |
| \`class-anychar\` | \`src/**/n*d[k-m]e?txt\` | character class + multiple wildcards |
| \`brace-anychar\` | \`src/**/{tob,crazy}/?*.{png,txt}\` | brace + \`?*\` + brace suffix |

### Multi-pattern test sets

| Set | Patterns |
|---|---|
| \`solo-globstar\` | \`["**/*.ts"]\` (degenerate union of one) |
| \`brace-equiv-4\` | 4 ext variants: \`**/*.{ts,tsx,js,jsx}\` written as separate patterns |
| \`brace-equiv-8\` | 8 ext variants |
| \`mixed-roots\` | \`["src/**/*.ts", "tests/**/*.ts", "lib/**/*.js"]\` (different roots) |
| \`huge-set-pos\` | 10 positives spanning roots / extensions |
| \`ignore-list-40\` | 40 ignore entries of unlike shapes (\`fixtures/bench-ignore-list.txt\`): chains for the match, a 38-state graph NFA for \`match_dir\` (a u64 in Rust, sparse in JS) |
| \`file-list-100\` | 100 literal files over 10 folders (\`src/<area>/mod_<i>.ts\`): literal paths, kept as one sorted set and answered by one search |
| \`gitignore-89\` | a project's .gitignore as globs (\`fixtures/bench-gitignore-list.txt\`): 89 directories, extensions and file names anywhere plus a few rooted entries; the like shapes merge into one chain each |
| \`global-586\` | a global .gitignore as globs (\`fixtures/bench-gitignore-global.txt\`, from alafourcadev/ultimate-gitignore): 533 file names, extensions and directories anywhere and 53 rooted entries, run as 62 chains and 14 literal paths |
| \`openssl-500\` | openssl's .gitignore as globs (\`fixtures/bench-gitignore-openssl.txt\`): 456 rooted entries, 436 of them literal paths, and 44 anywhere; 415 literal paths in one sorted set, 21 chains for the rest |
| \`joomla-705\` | the Joomla template of github/gitignore as globs (\`fixtures/bench-gitignore-joomla.txt\`): 705 rooted entries, 446 literal paths and 258 folders' children (\`dir/*\`); 437 literal paths in one sorted set, 33 chains for the rest |
| \`all-cases\` | every pattern of this bench but the three real .gitignore sets in one union, each once: its 100 literal paths as one set, 25 chains for the rest — the widest, most mixed set, and the shape where a per-member matcher loses to a DFA |

Each library matches a set the way it offers to: globstar through
\`Glob::union\` / \`globstar(list)\`, globset through one \`GlobSet\`, wax
through \`wax::any\`, picomatch and micromatch through their list form
(their matcher tries the members in order and stops at the first hit).
minimatch and the fast-glob crate have no list form, so their columns loop
one single-pattern matcher per member the same way, as a caller would.

wax compiles the whole set into one regex and matches it with a lazy DFA:
its match cost is flat in the member count, and it pays for that at build
(40× globstar's on these sets) and in memory (25×). globstar matches a
set through its chains or its NFA, whose cost grows with the members, so
on the widest mixed set (\`ignore-list-40\`) wax's match is the faster
one. Its \`file-list-100\` match is a length check: the regex crate
rejects a path shorter or longer than any match can be, and no bench
path is as long as a member (15–20 bytes).

### Match input — ${PATHS.length} paths (identical on both runtimes)

\`\`\`
${PATHS.join("\n")}
\`\`\`

### Walker tree shapes

Walker rows are labeled \`<scale>_<pattern-set>\`. Three temp-dir trees
are built once and kept between runs, with identical shape on both sides.

| Scale | ~Files | Generator parameters (\`packages/bench/benches/walker.js\`) |
|---|---|---|
| \`small\` | ~120 | 1 area × 5 modules × 1 sub; 5 tests; 5 pkgs × 5 files; 10 dist chunks |
| \`medium\` | ~3,700 | 3 areas × 20 modules × 5 subs; 50 tests; 50 pkgs × 20 files; 100 dist chunks |
| \`large\` | ~25,000 | 5 areas × 50 modules × 10 subs; 200 tests; 200 pkgs × 30 files; 500 dist chunks |

Two pattern sets per scale: \`multi-small\` (4 patterns) and
\`multi-huge\` (10 patterns).
`);

sections.push(`## Single-pattern matcher

### Build (per matcher)

${combinedTable("Pattern", SINGLE_PATTERNS, [rustSingle.build, jsSingle.build])}

### Build + first non-match

${combinedTable("Pattern", SINGLE_PATTERNS, [rustSingle.compile, jsSingle.compile])}

### Match (per call)

${combinedTable("Pattern", SINGLE_PATTERNS, [rustSingle.match, jsSingle.match])}

### Memory (KB / matcher)

${combinedTable("Pattern", SINGLE_PATTERNS, [rustSingle.mem, jsSingle.mem])}
`);

sections.push(`## Multi-pattern matcher

### Build (per matcher)

${combinedTable("Set", MULTI_SETS, [rustMulti.build, jsMulti.build])}

### Build + first non-match

${combinedTable("Set", MULTI_SETS, [rustMulti.compile, jsMulti.compile])}

### Match (per call)

${combinedTable("Set", MULTI_SETS, [rustMulti.match, jsMulti.match])}

### Memory (KB / matcher)

${combinedTable("Set", MULTI_SETS, [rustMulti.mem, jsMulti.mem])}
`);

sections.push(`## Walker (end-to-end)

${combinedTable("Tree × patterns", WALKER_LABELS, [rustWalker, jsWalker])}
`);

const report = sections.join("\n");
if (missingCells.length > 0 && !(SKIP_RUST || SKIP_JS)) {
  process.stderr.write(`\n[bench] no number for: ${missingCells.join(", ")}\n`);
  failed = true;
}
if (failed) {
  process.stderr.write("\n[bench] a step failed; BENCHMARKS.md not written\n");
  process.exit(1);
}
if (SKIP_RUST || SKIP_JS) {
  // Half a run is for reading, not for the committed report.
  process.stdout.write(report);
} else {
  writeFileSync(resolve(ROOT, "BENCHMARKS.md"), report);
  process.stderr.write(`\n[bench] wrote ${resolve(ROOT, "BENCHMARKS.md")}\n`);
}
