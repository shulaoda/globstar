#!/usr/bin/env node
// One-click benchmark runner.
//
// Compares globstar (default public API) against the dominant
// ecosystem libraries on three axes (compile / match / memory) for
// both single-pattern and multi-pattern matchers, plus end-to-end
// walker timings. Rust + JS columns are merged into single tables so
// rows can be read across runtimes.
//
//   node bench.mjs              # full run (~5 min on Apple Silicon)
//   node bench.mjs --skip-rust  # JS only
//   node bench.mjs --skip-js    # Rust only

import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = dirname(fileURLToPath(import.meta.url));
process.chdir(ROOT);

const args = new Set(process.argv.slice(2));
const SKIP_RUST = args.has("--skip-rust");
const SKIP_JS = args.has("--skip-js");

// Number of paths in the matcher's `match` corpus. Rust criterion
// times the whole batch per iter; we divide by this to bring it onto
// the per-call basis JS mitata reports natively.
const PATHS_PER_BATCH = 11;

// Single-pattern row labels (must match both engines.rs and
// `_engines_compare.js`).
const SINGLE_PATTERNS = [
  "literal",
  "simple-wildcard",
  "globstar",
  "brace-suffix",
  "reject-prefilter",
  "class-anychar",
  "brace-anychar",
];

// Multi-pattern row labels (must match both multi.rs and
// `_engines_compare_multi.js`).
const MULTI_SETS = [
  "solo-globstar",
  "brace-equiv-4",
  "brace-equiv-8",
  "mixed-roots",
  "huge-set-pos",
];

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

function step(name, cmd, argv) {
  process.stderr.write(`\n[bench] ${name} → ${cmd} ${argv.join(" ")}\n`);
  const t0 = Date.now();
  const res = spawnSync(cmd, argv, {
    stdio: ["inherit", "pipe", "pipe"],
    encoding: "utf-8",
  });
  const dt = ((Date.now() - t0) / 1000).toFixed(1);
  if (res.status !== 0) {
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
  return `${ns.toFixed(2)} ns`;
}

function formatCell(val, opts = {}) {
  if (!val || val === "—") return val;
  const ns = timeToNs(val);
  if (ns !== null) {
    const scaled = opts.divideBy ? ns / opts.divideBy : ns;
    return nsToString(scaled);
  }
  // Bare integer (Rust memory tools).
  if (/^\d+$/.test(val) && opts.isMem) return `${val} B`;
  // JS memory: `424 B` style — round + ensure integer.
  const m = val.match(/^([\d.]+)\s*B$/);
  if (m) return `${Math.round(parseFloat(m[1]))} B`;
  return val;
}

// ── runners ──────────────────────────────────────────────────────

const rust = {};
const js = {};

if (!SKIP_RUST) {
  step("rust build (release)", "cargo", ["build", "--release", "--workspace"]);
  step("rust build memory tools", "cargo", ["build", "--release", "-p", "memory-check"]);
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
    step("rust single memory", "./target/release/matcher-single-memory", []),
    "Pattern",
  );
  rust.multiMem = parseTable(
    step("rust multi memory", "./target/release/matcher-multi-memory", []),
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
function combinedTable(rowLabelHeader, rowLabels, sources) {
  const headers = [rowLabelHeader, ...sources.flatMap((s) => s.libCols.map((c) => c[0]))];
  const rows = rowLabels.map((label) => {
    const cells = sources.flatMap((src) => {
      const groupKey = src.rowKey(label);
      const group = src.data[groupKey] ?? {};
      return src.libCols.map(([_, key]) => formatCell(group[key] ?? "—", src.cellOpts ?? {}));
    });
    return [label, ...cells];
  });
  return table(headers, rows);
}

// ── source helpers ──────────────────────────────────────────────

const empty = {};

const rustSingle = {
  compile: {
    data: rust.matcherSingle ?? empty,
    rowKey: (l) => `compile_${l}`,
    libCols: [
      ["globstar (Rust)", "globstar"],
      ["globset (Rust)", "globset"],
      ["wax (Rust)", "wax"],
    ],
  },
  match: {
    data: rust.matcherSingle ?? empty,
    rowKey: (l) => `match_${l}`,
    libCols: [
      ["globstar (Rust)", "globstar"],
      ["globset (Rust)", "globset"],
      ["wax (Rust)", "wax"],
      ["fast_glob (Rust)", "fast_glob"],
    ],
    cellOpts: { divideBy: PATHS_PER_BATCH },
  },
  mem: {
    data: rust.singleMem ?? empty,
    rowKey: (l) => l,
    libCols: [
      ["globstar (Rust)", "globstar"],
      ["globset (Rust)", "globset"],
      ["wax (Rust)", "wax"],
    ],
    cellOpts: { isMem: true },
  },
};

const jsSingle = {
  compile: {
    data: js.matcherSingle ?? empty,
    rowKey: (l) => `compile: ${l}`,
    libCols: [
      ["globstar (JS)", "globstar"],
      ["picomatch (JS)", "picomatch"],
      ["minimatch (JS)", "minimatch"],
      ["micromatch (JS)", "micromatch"],
    ],
  },
  match: {
    data: js.matcherSingle ?? empty,
    rowKey: (l) => `match: ${l}`,
    libCols: [
      ["globstar (JS)", "globstar"],
      ["picomatch (JS)", "picomatch"],
      ["minimatch (JS)", "minimatch"],
      ["micromatch (JS)", "micromatch"],
    ],
  },
  mem: {
    data: js.singleMem ?? empty,
    rowKey: (l) => l,
    libCols: [
      ["globstar (JS)", "globstar"],
      ["picomatch (JS)", "picomatch"],
      ["minimatch (JS)", "minimatch"],
      ["micromatch (JS)", "micromatch"],
    ],
    cellOpts: { isMem: true },
  },
};

const rustMulti = {
  compile: {
    data: rust.matcherMulti ?? empty,
    rowKey: (l) => `compile_${l}`,
    libCols: [
      ["globstar (Rust)", "globstar"],
      ["globset (Rust)", "globset"],
      ["wax (Rust)", "wax_or"],
    ],
  },
  match: {
    data: rust.matcherMulti ?? empty,
    rowKey: (l) => `match_${l}`,
    libCols: [
      ["globstar (Rust)", "globstar"],
      ["globset (Rust)", "globset"],
      ["wax (Rust)", "wax_or"],
      ["fast_glob (Rust)", "fast_glob_or"],
    ],
    cellOpts: { divideBy: PATHS_PER_BATCH },
  },
  mem: {
    data: rust.multiMem ?? empty,
    rowKey: (l) => l,
    libCols: [
      ["globstar (Rust)", "gs_union"],
      ["globset (Rust)", "globset"],
      ["wax (Rust)", "wax_per"],
    ],
    cellOpts: { isMem: true },
  },
};

const jsMulti = {
  compile: {
    data: js.matcherMulti ?? empty,
    rowKey: (l) => `compile: ${l}`,
    libCols: [
      ["globstar (JS)", "globstar"],
      ["picomatch (JS)", "picomatch"],
      ["minimatch (JS)", "minimatch"],
      ["micromatch (JS)", "micromatch"],
    ],
  },
  match: {
    data: js.matcherMulti ?? empty,
    rowKey: (l) => `match: ${l}`,
    libCols: [
      ["globstar (JS)", "globstar"],
      ["picomatch (JS)", "picomatch"],
      ["minimatch (JS)", "minimatch"],
      ["micromatch (JS)", "micromatch"],
    ],
  },
  mem: {
    data: js.multiMem ?? empty,
    rowKey: (l) => l,
    libCols: [
      ["globstar (JS)", "globstar"],
      ["picomatch (JS)", "picomatch"],
      ["minimatch (JS)", "minimatch"],
      ["micromatch (JS)", "micromatch"],
    ],
    cellOpts: { isMem: true },
  },
};

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

Generated by \`node bench.mjs\` on ${new Date().toISOString().slice(0, 19)}.

Three axes (\`compile\` / \`match\` / \`memory\`) for both single- and
multi-pattern matchers, plus end-to-end walker timings. Rust and JS
columns share each table — every row exercises the **same pattern and
match input on both runtimes**. Numbers are medians from criterion
(Rust) / mitata (JS); use them for relative comparison, not absolute
SLOs.

> **Match cell semantics.** Rust criterion times the full 11-path
> batch per iter; JS mitata times one match call per iter. The
> orchestrator divides the Rust value by 11 so all \`match\` columns
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
| \`brace-anychar\` | \`src/**/{tob,crazy}/?*.{png,txt}\` | brace + class + complex tail |

### Multi-pattern test sets

| Set | Patterns |
|---|---|
| \`solo-globstar\` | \`["**/*.ts"]\` (degenerate union of one) |
| \`brace-equiv-4\` | 4 ext variants: \`**/*.{ts,tsx,js,jsx}\` written as separate patterns |
| \`brace-equiv-8\` | 8 ext variants |
| \`mixed-roots\` | \`["src/**/*.ts", "tests/**/*.ts", "lib/**/*.js"]\` (different roots) |
| \`huge-set-pos\` | 10 positives spanning roots / extensions |

### Match input — 11 paths (identical on both runtimes)

\`\`\`
src/main.ts
src/cli/commands/build.ts
src/cli/commands/run.rs
src/main.rs
tests/smoke.ts
dist/bundle.js
node_modules/foo/index.js
package.json
.env
target/debug/build/foo
src/a/bigger/path/to/the/crazy/needle.txt
\`\`\`

### Walker tree shapes

Walker rows are labeled \`<scale>_<pattern-set>\`. Three temp-dir trees
are seeded once per run with identical shape on both sides.

| Scale | ~Files | Shape |
|---|---|---|
| \`small\` | ~120 | 1 area × 5 modules × 4 ext + 5×2 tests + 5 pkgs × 5 files + 10×2 dist |
| \`medium\` | ~3,700 | 3 areas × 20 modules × 5 subs + 50×2 tests + 50 pkgs × 20 files + 100×2 dist |
| \`large\` | ~25,000 | 5 areas × 50 modules × 10 subs + 200×2 tests + 200 pkgs × 30 files + 500×2 dist |

Two pattern sets per scale: \`multi-small\` (4 patterns) and
\`multi-huge\` (10 patterns).
`);

sections.push(`## Single-pattern matcher

### Compile (per matcher)

${combinedTable("Pattern", SINGLE_PATTERNS, [rustSingle.compile, jsSingle.compile])}

### Match (per call)

${combinedTable("Pattern", SINGLE_PATTERNS, [rustSingle.match, jsSingle.match])}

### Memory (B / matcher)

${combinedTable("Pattern", SINGLE_PATTERNS, [rustSingle.mem, jsSingle.mem])}
`);

sections.push(`## Multi-pattern matcher

### Compile (per matcher)

${combinedTable("Set", MULTI_SETS, [rustMulti.compile, jsMulti.compile])}

### Match (per call)

${combinedTable("Set", MULTI_SETS, [rustMulti.match, jsMulti.match])}

### Memory (B / matcher)

${combinedTable("Set", MULTI_SETS, [rustMulti.mem, jsMulti.mem])}
`);

sections.push(`## Walker (end-to-end)

${combinedTable("Tree × patterns", WALKER_LABELS, [rustWalker, jsWalker])}
`);

const report = sections.join("\n");
writeFileSync(resolve(ROOT, "BENCHMARKS.md"), report);
process.stderr.write(`\n[bench] wrote ${resolve(ROOT, "BENCHMARKS.md")}\n`);
