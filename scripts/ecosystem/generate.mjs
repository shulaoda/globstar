#!/usr/bin/env node
// Regenerates `fixtures/ecosystem/`: the cases other glob libraries assert
// in their own test suites, answered by globstar and by every library the
// benchmarks compare against, at the versions the workspace pins. Per
// suite: `<side>/<lib>.txt` (rows where globstar and the library agree),
// `<side>/<lib>-diff.txt` (rows where they differ, grouped by reason, the
// library's answer in a comment), and the README with the counts and the
// cross-library matrix. The suites are fetched once into
// `node_modules/.cache/ecosystem/`; the Rust libraries answer through
// `tools/ecosystem`.
//
//   node scripts/ecosystem/generate.mjs

import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { SOURCES, fetchSource } from "./sources.mjs";
import { harvestMinimatch, harvestMocha, wrapMicromatch, wrapPicomatch } from "./harvest-js.mjs";
import { harvestFastGlob, harvestGlobset, harvestWax } from "./harvest-rust.mjs";
import { reasonFor } from "./reasons.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const OUT = join(ROOT, "fixtures/ecosystem");
const CACHE = join(ROOT, "node_modules/.cache/ecosystem");
const require = createRequire(join(ROOT, "packages/bench/package.json"));

// Which options each library can be asked about: a row needing one a
// library lacks is `n/a` for it, unless the option cannot change the
// answer (`dot` only matters when a name starts with `.`).
const LIBS = [
  { id: "globstar", side: "", dot: true, ci: true },
  { id: "picomatch", side: "js", dot: true, ci: true },
  { id: "micromatch", side: "js", dot: true, ci: true },
  { id: "minimatch", side: "js", dot: true, ci: true },
  { id: "globset", side: "rust", dot: false, ci: true },
  { id: "fast-glob", side: "rust", dot: false, ci: false },
  { id: "wax", side: "rust", dot: false, ci: false },
];
const PROBE_ORDER = ["globstar", "globset", "fast-glob", "wax"];

// Every library runs in the configuration closest to this dialect. A
// suite's test that sets one of these options to this value is imported;
// one that sets it to the other value tests the other behavior and is not.
const CONFIG = {
  picomatch: {
    options: { strictSlashes: true },
    text: "`strictSlashes: true`: a trailing `/` is a boundary and `a/**` does not match `a` (GLOB_SPEC §9.5, §14.2)",
  },
  micromatch: {
    options: { strictSlashes: true },
    text: "`strictSlashes: true`: a trailing `/` is a boundary and `a/**` does not match `a` (GLOB_SPEC §9.5, §14.2)",
  },
  minimatch: {
    options: { preserveMultipleSlashes: true, optimizationLevel: 0 },
    text: "`preserveMultipleSlashes: true`: `//` is two boundaries (GLOB_SPEC §14.2); `optimizationLevel: 0`: `.` and `..` are plain names (§3.1)",
  },
  globset: {
    options: {},
    text: "`literal_separator(true)`: `*` stops at `/` (GLOB_SPEC §9.1); `backslash_escape(true)`: `\\` escapes (§4); `empty_alternates(true)`: an empty branch is a branch (§6.1)",
  },
  "fast-glob": { options: {}, text: "no options" },
  wax: { options: {}, text: "no options" },
};

const hasDotName = (path) => /(^|\/)\.[^/]/.test(path);
const comparable = (lib, row) =>
  (!row.ci || lib.ci) && (row.dot || lib.dot || !hasDotName(row.path));

const jsVersion = (name) => require(`${name}/package.json`).version;
const lock = readFileSync(join(ROOT, "Cargo.lock"), "utf8");
const lockVersions = (name) =>
  [
    ...lock.matchAll(
      new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`, "g"),
    ),
  ].map((m) => m[1]);
const RUNNING = {
  globstar: JSON.parse(readFileSync(join(ROOT, "packages/globstar/package.json"), "utf8")).version,
  picomatch: jsVersion("picomatch"),
  micromatch: jsVersion("micromatch"),
  minimatch: jsVersion("minimatch"),
  globset: lockVersions("globset")[0],
  wax: lockVersions("wax")[0],
  "fast-glob": lockVersions("fast-glob")[0],
};
for (const name of ["globset", "wax", "fast-glob"]) {
  if (lockVersions(name).length !== 1)
    throw new Error(`${name}: Cargo.lock holds more than one version`);
}
for (const s of SOURCES) {
  const running = RUNNING[s.id];
  if (running !== s.version) {
    throw new Error(
      `${s.id}: the suite is pinned at ${s.version} but the workspace runs ${running}`,
    );
  }
}

const dirs = {};
for (const s of SOURCES) dirs[s.id] = await fetchSource(s, CACHE);

const quiet = async (f) => {
  const log = console.log;
  console.log = () => {};
  try {
    return await f();
  } finally {
    console.log = log;
  }
};
const HARVEST = {
  picomatch: () =>
    harvestMocha({
      dir: dirs.picomatch,
      real: require("picomatch"),
      wrap: wrapPicomatch,
      extra: { minimatch: require("minimatch") },
    }),
  micromatch: () =>
    harvestMocha({
      dir: dirs.micromatch,
      real: require("micromatch"),
      wrap: wrapMicromatch,
      extra: {
        "fill-range": () => {
          throw new Error("fill-range is not installed");
        },
      },
    }),
  minimatch: () => harvestMinimatch(dirs.minimatch),
  globset: () => harvestGlobset(dirs.globset),
  "fast-glob": () => harvestFastGlob(dirs["fast-glob"]),
  wax: () => harvestWax(dirs.wax),
};

// Options the JS suites pass that cannot change an `isMatch` answer.
const JS_IGNORED = new Set(["nonull", "null", "matches"]);
// globset's option constants: what each means for the row.
const GLOBSET_OPTS = { SLASHLIT: {}, BSESC: {}, CASEI: { ci: true } };
// A pattern or path past this length is a stress test, not a case.
const MAX_ROW_BYTES = 512;
const EXTGLOB = /[@!+?*]\(/;
const POSIX_CLASS = /\[\[:/;

// A harvested case as a row, or dropped with a reason.
function normalize(source, c) {
  const drop = (why) => ({ drop: why });
  if (c.pattern.length > MAX_ROW_BYTES || c.path.length > MAX_ROW_BYTES)
    return drop(`longer than ${MAX_ROW_BYTES} bytes`);
  if (EXTGLOB.test(c.pattern)) return drop("syntax: extglob");
  if (POSIX_CLASS.test(c.pattern)) return drop("syntax: POSIX class");
  let dot = true;
  let ci = false;
  if (source.side === "js") {
    // The three libraries' default.
    dot = false;
    const options =
      c.options && typeof c.options === "object" && !Array.isArray(c.options) ? c.options : {};
    const aligned = CONFIG[source.id].options;
    for (const [k, v] of Object.entries(options)) {
      if (v === undefined || v === null) continue;
      if (k === "dot") dot = Boolean(v);
      else if (k === "nocase") ci = Boolean(v);
      else if (k in aligned) {
        if (v !== aligned[k]) return drop(`option: ${k}=${JSON.stringify(v)}`);
      } else if (!JS_IGNORED.has(k)) return drop(`option: ${k}`);
    }
  } else if (source.id === "globset" && c.options.globset) {
    const o = GLOBSET_OPTS[c.options.globset];
    if (!o) return drop(`option: ${c.options.globset}`);
    if (o.ci) ci = true;
  }
  return {
    row: {
      pattern: c.pattern,
      path: c.path,
      dot,
      ci,
      at: c.at,
      group: c.group,
      kind: c.kind ?? "match",
    },
  };
}

const { compileMatcher } = await import(join(ROOT, "packages/globstar/src/glob.js"));
const { GlobError } = await import(join(ROOT, "packages/globstar/src/error.js"));
const pm = require("picomatch");
const mm = require("micromatch");
const { minimatch } = await import(require.resolve("minimatch"));

const verdict = (b) => (b ? "match" : "no-match");
const attempt = (f) => {
  try {
    return verdict(f());
  } catch (e) {
    return {
      err: String(e && e.message ? e.message : e)
        .split("\n")[0]
        .slice(0, 80),
    };
  }
};
const JS_ANSWER = {
  globstar: (r) => {
    try {
      return verdict(
        compileMatcher(r.pattern, { dot: r.dot, caseInsensitive: r.ci }).match(r.path),
      );
    } catch (e) {
      if (e instanceof GlobError) return `err:${e.kind}`;
      throw e;
    }
  },
  picomatch: (r) =>
    attempt(() =>
      pm.isMatch(r.path, r.pattern, { dot: r.dot, nocase: r.ci, ...CONFIG.picomatch.options }),
    ),
  micromatch: (r) =>
    attempt(() =>
      mm.isMatch(r.path, r.pattern, { dot: r.dot, nocase: r.ci, ...CONFIG.micromatch.options }),
    ),
  minimatch: (r) =>
    attempt(() =>
      minimatch(r.path, r.pattern, { dot: r.dot, nocase: r.ci, ...CONFIG.minimatch.options }),
    ),
};

// The probe's wire escapes, and a corpus field (a tab is `⇥`, a newline `⏎`).
const wire = (s) => s.replaceAll("\\", "\\\\").replaceAll("\t", "\\t").replaceAll("\n", "\\n");
const esc = (s) => s.replaceAll("\t", "⇥").replaceAll("\n", "⏎");

function rustAnswers(rows) {
  execFileSync("cargo", ["build", "-q", "--release", "-p", "ecosystem"], {
    cwd: ROOT,
    stdio: "inherit",
  });
  const input =
    rows
      .map((r) => `${r.dot ? 1 : 0}${r.ci ? 1 : 0}\t${wire(r.pattern)}\t${wire(r.path)}`)
      .join("\n") + "\n";
  const res = spawnSync(join(ROOT, "target/release/ecosystem-probe"), {
    input,
    encoding: "utf8",
    maxBuffer: 1 << 28,
  });
  if (res.status !== 0) throw new Error(`ecosystem-probe failed: ${res.stderr}`);
  const lines = res.stdout.split("\n").filter(Boolean);
  if (lines.length !== rows.length)
    throw new Error(`ecosystem-probe answered ${lines.length} of ${rows.length}`);
  return lines.map((line) => {
    const cols = line.split("\t");
    const out = {};
    PROBE_ORDER.forEach((id, i) => {
      const t = cols[i];
      out[id] =
        t === "match" || t === "no-match" || t.startsWith("err:")
          ? t
          : { err: t.replace(/^error:/, "") };
    });
    return out;
  });
}

const suites = [];
for (const source of SOURCES) {
  const h = await quiet(() => Promise.resolve(HARVEST[source.id]()));
  const dropped = {};
  for (const [k, v] of Object.entries(h.skipped))
    if (v)
      dropped[
        k === "arrayPattern"
          ? "a list of patterns"
          : k === "acceptOnly"
            ? "compiles-only assertion (no path)"
            : k
      ] = v;
  const seen = new Set();
  const rows = [];
  let duplicates = 0;
  for (const c of h.cases) {
    const n = normalize(source, c);
    if (n.drop) {
      dropped[n.drop] = (dropped[n.drop] ?? 0) + 1;
      continue;
    }
    const key = JSON.stringify([n.row.pattern, n.row.path, n.row.dot, n.row.ci]);
    if (seen.has(key)) {
      duplicates++;
      continue;
    }
    seen.add(key);
    rows.push(n.row);
  }
  suites.push({
    source,
    rows,
    dropped,
    duplicates,
    harvested: h.cases.length,
    failures: h.failures,
  });
}

const allRows = suites.flatMap((s) => s.rows);
const rust = rustAnswers(allRows);
allRows.forEach((row, i) => {
  row.answers = {};
  for (const lib of LIBS) {
    if (lib.id in JS_ANSWER) row.answers[lib.id] = JS_ANSWER[lib.id](row);
    else row.answers[lib.id] = rust[i][lib.id];
  }
  if (rust[i].globstar !== row.answers.globstar) {
    throw new Error(
      `globstar disagrees with itself on ${JSON.stringify(row.pattern)} / ${JSON.stringify(row.path)}: JS ${row.answers.globstar}, Rust ${rust[i].globstar}`,
    );
  }
});

// `agree`, `agree-error` (both refuse), `we-reject`, `they-reject`,
// `disagree`, or `n/a` when the library cannot be asked.
function status(lib, row) {
  if (!comparable(lib, row)) return "n/a";
  const ours = row.answers.globstar;
  const theirs = row.answers[lib.id];
  const oursErr = ours.startsWith("err:");
  const theirsErr = typeof theirs !== "string";
  if (oursErr && theirsErr) return "agree-error";
  if (oursErr) return "we-reject";
  if (theirsErr) return "they-reject";
  return ours === theirs ? "agree" : "disagree";
}

const sourceLib = (s) => s.id;
const showAnswer = (a) => (typeof a === "string" ? a : `error (${a.err})`);
const flagsOf = (row) => {
  const f = [];
  if (!row.dot) f.push("dot=false");
  if (row.ci) f.push("case_insensitive=true");
  return f.length ? "\t" + f.join(",") : "";
};
const tsv = (row) =>
  `${esc(row.pattern)}\t${esc(row.path)}\t${row.answers.globstar}${flagsOf(row)}`;

// The upstream test's name as a section id, cut at a word past 60 chars.
function groupId(title) {
  const id = title
    .replace(/[^A-Za-z0-9]+/g, "_")
    .replace(/^_|_$/g, "")
    .toLowerCase();
  if (id.length <= 60) return id || "cases";
  const cut = id.lastIndexOf("_", 60);
  return cut > 20 ? id.slice(0, cut) : id.slice(0, 60);
}

function header(source, kind, suite, counts) {
  const lib = sourceLib(source);
  const lines = [
    `# ${source.id}${kind === "diff" ? "-diff" : ""}.txt — generated by scripts/ecosystem/generate.mjs; do not edit.`,
    `# Source: ${source.tree}`,
    `#   ${source.id} ${source.version}${source.note ? ` (${source.note})` : ""}`,
    `# Answers: ${source.id} ${RUNNING[lib]} and globstar ${RUNNING.globstar}, run on every case.`,
  ];
  if (source.side === "js")
    lines.push("# Rows without a flag run with dot=false, the library's default.");
  if (CONFIG[source.id].text !== "no options")
    lines.push(`# ${source.id} runs with ${CONFIG[source.id].text}.`);
  lines.push(
    kind === "diff"
      ? `# Rows where globstar and ${source.id} differ, by reason; each row asserts globstar's answer and`
      : `# Rows where globstar and ${source.id} agree (both answers, or both refusing the pattern).`,
  );
  if (kind === "diff")
    lines.push(
      `# the comment above it gives ${source.id}'s. Format: PATTERN<TAB>PATH<TAB>EXPECTED[<TAB>FLAGS].`,
    );
  else
    lines.push(
      "# Format: PATTERN<TAB>PATH<TAB>EXPECTED[<TAB>FLAGS]; see fixtures/corpus/README.md.",
    );
  lines.push(`# ${counts}`);
  return lines.join("\n") + "\n";
}

mkdirSync(join(OUT, "js"), { recursive: true });
mkdirSync(join(OUT, "rust"), { recursive: true });

const summary = [];
for (const suite of suites) {
  const { source, rows } = suite;
  const lib = LIBS.find((l) => l.id === sourceLib(source));
  const counts = { agree: 0, "agree-error": 0, disagree: 0, "we-reject": 0, "they-reject": 0 };
  const agreeByGroup = new Map();
  const diffByReason = new Map();
  const reasons = {};
  for (const row of rows) {
    const st = status(lib, row);
    counts[st]++;
    if (st === "agree" || st === "agree-error") {
      if (!agreeByGroup.has(row.group)) agreeByGroup.set(row.group, []);
      agreeByGroup.get(row.group).push({ row, st });
    } else {
      const reason = reasonFor(row, row.answers.globstar, row.answers[lib.id]);
      reasons[reason.id] = reason;
      if (!diffByReason.has(reason.id)) diffByReason.set(reason.id, []);
      diffByReason.get(reason.id).push(row);
    }
  }
  const total = rows.length;
  const same = counts.agree + counts["agree-error"];
  const countLine = `${total} rows: ${same} agree (${counts["agree-error"]} as a refusal), ${counts.disagree} disagree, ${counts["we-reject"]} globstar refuses, ${counts["they-reject"]} ${source.id} refuses.`;

  let main = header(source, "main", suite, countLine);
  for (const [group, list] of agreeByGroup) {
    main += `\n## ${source.id}.${groupId(group)}\n`;
    for (const { row, st } of list) {
      if (st === "agree-error") main += `# ${source.id}: ${showAnswer(row.answers[lib.id])}\n`;
      main += tsv(row) + "\n";
    }
  }
  writeFileSync(join(OUT, source.side, `${source.id}.txt`), main);

  let diff = header(source, "diff", suite, countLine);
  const order = [...diffByReason.keys()].sort(
    (a, b) => diffByReason.get(b).length - diffByReason.get(a).length,
  );
  for (const id of order) {
    const reason = reasons[id];
    const list = diffByReason.get(id);
    diff += `\n## ${source.id}.diff.${id}\n# ${reason.title} (${list.length} row${list.length === 1 ? "" : "s"})\n`;
    for (const line of wrap(reason.text, 76)) diff += `# ${line}\n`;
    // A refusal does not depend on the path: one row per pattern.
    const refusal = reason.kind !== "disagree";
    const shown = refusal ? collapse(list) : list.map((row) => ({ row, more: 0 }));
    if (reason.kind === "they-reject")
      shown.sort((a, b) => cmp(a.row.answers[lib.id].err, b.row.answers[lib.id].err));
    for (const { row, more } of shown) {
      const andMore = more ? `  (and ${more} more path${more === 1 ? "" : "s"})` : "";
      diff += `# ${source.id}: ${showAnswer(row.answers[lib.id])}  [${row.at}]${andMore}\n`;
      diff += tsv(row) + "\n";
    }
  }
  writeFileSync(join(OUT, source.side, `${source.id}-diff.txt`), diff);
  summary.push({
    suite,
    counts,
    reasons: order.map((id) => [id, reasons[id].title, diffByReason.get(id).length]),
  });
}

function cmp(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

// One entry per (pattern, flags), with how many other paths it stood for.
function collapse(rows) {
  const out = new Map();
  for (const row of rows) {
    const key = JSON.stringify([row.pattern, row.dot, row.ci]);
    if (out.has(key)) out.get(key).more++;
    else out.set(key, { row, more: 0 });
  }
  return [...out.values()];
}

function wrap(text, width) {
  const words = text.split(" ");
  const lines = [];
  let line = "";
  for (const w of words) {
    if (line && line.length + 1 + w.length > width) {
      lines.push(line);
      line = w;
    } else line = line ? `${line} ${w}` : w;
  }
  if (line) lines.push(line);
  return lines;
}

const pct = (a, b) => (b === 0 ? "–" : `${Math.round((1000 * a) / b) / 10}%`);
let md = `# Ecosystem corpus

The cases other glob libraries assert in their own test suites, answered
by globstar and by every library the benchmarks compare against, at the
versions this workspace pins. Generated by \`scripts/ecosystem/generate.mjs\`;
do not edit the files by hand.

Every row is a corpus row (\`PATTERN<TAB>PATH<TAB>EXPECTED[<TAB>FLAGS]\`,
see [\`../corpus/README.md\`](../corpus/README.md)) that asserts **globstar's**
answer, so \`crates/globstar/tests/corpus.rs\` and \`scripts/verify.mjs\` run
them like the spec corpus. Per suite there are two files: \`<lib>.txt\` holds
the rows where globstar and the library agree (both answers, or both
refusing the pattern), \`<lib>-diff.txt\` the rows where they differ, grouped
by reason, with the library's answer above each row.

The answers come from running the libraries, not from the suites'
expectations, so a row says what each library does at that version. Each
library is asked only what it can express: a \`case_insensitive\` row is not
put to a library without the option, and a \`dot=false\` row is not put to a
library without the option when a name in the path starts with \`.\`. The
three JS libraries default to \`dot: false\`, so their rows carry
\`dot=false\` unless the test set \`dot\`.

## Configuration

Every library runs in the configuration closest to this dialect, so what
is left differs in meaning, not in settings. A test that sets one of these
options to this value is imported; one that sets it to the other value
tests the other behavior and is not.

| Library | Runs with |
| --- | --- |
${SOURCES.map((s) => `| ${s.id} | ${CONFIG[s.id].text} |`).join("\n")}

Not imported, and counted below: syntax globstar does not have (extglobs,
POSIX classes) and options it does not have (\`windows\`, \`bash\`,
\`ignore\`, \`matchBase\`, \`noglobstar\`, …). A test that passes a list
of patterns is a union and is not a single-pattern row.

The suites were run on POSIX, where the libraries behave as recorded
(picomatch, for one, changes its rules on Windows), so the runners skip
these files on Windows. Regenerate with \`just ecosystem\`; the first run
fetches the suites and needs the network and a Rust toolchain.

## Reading a diff file

\`\`\`
## picomatch.diff.trailing-globstar
# A trailing \`/**\` and the directory itself (10 rows)
# \`a/**\` needs the \`a/\` prefix: it matches \`a/\` and everything below, not
# \`a\` itself (GLOB_SPEC §9.5). picomatch and micromatch match \`a\` too …
# picomatch: match  [test/globstars.js:39]
a/**<TAB>a<TAB>no-match<TAB>dot=false
\`\`\`

A group names the reason, says how many rows it holds and explains both
behaviors; each row is then the library's answer and where the suite
asserts it, above the corpus row with globstar's answer. A pattern a
library refuses, or globstar refuses, is listed once with the number of
other paths the suite tried it on.

## Suites

| Suite | Version | Files | Harvested | Rows | Agree | Disagree | globstar refuses | Library refuses |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
`;
for (const { suite, counts } of summary) {
  const { source, rows, harvested } = suite;
  const same = counts.agree + counts["agree-error"];
  const files = `[agree](${source.side}/${source.id}.txt) · [diff](${source.side}/${source.id}-diff.txt)`;
  md += `| [${source.id}](${source.tree}) | ${source.version} | ${files} | ${harvested} | ${rows.length} | ${same} (${pct(same, rows.length)}) | ${counts.disagree} | ${counts["we-reject"]} | ${counts["they-reject"]} |\n`;
}
md += `
"Harvested" counts every assertion the suite makes, duplicates included;
"Rows" is what is left after the drops below and after merging identical
(pattern, path, flags) cases. Agree includes the rows both sides refuse.

## What was not imported

`;
for (const { suite } of summary) {
  const { source, dropped, duplicates } = suite;
  const parts = Object.entries(dropped)
    .sort((a, b) => b[1] - a[1])
    .map(([k, v]) => `${k} (${String(v)})`);
  md += `- **${source.id}**: ${parts.length ? parts.join(", ") : "nothing"}; ${duplicates} duplicate rows merged.\n`;
}
md += `
## Why they differ

The reasons, per suite, most frequent first; the \`-diff.txt\` files hold the
rows and a paragraph on each reason.

`;
for (const { suite, reasons } of summary) {
  md += `- **${suite.source.id}**: ${reasons.length ? reasons.map(([, title, n]) => `${title} (${n})`).join(", ") : "no differences"}.\n`;
}
md += `
## Across the ecosystem

How often each library gives the same answer as the library whose suite
the rows came from (two refusals count as the same answer). A cell counts
only the rows the library can be asked about.

| Suite | ${LIBS.map((l) => l.id).join(" | ")} |
| --- | ${LIBS.map(() => "---:").join(" | ")} |
`;
for (const suite of suites) {
  const own = LIBS.find((l) => l.id === sourceLib(suite.source));
  const cells = LIBS.map((lib) => {
    let agree = 0;
    let n = 0;
    for (const row of suite.rows) {
      if (!comparable(lib, row) || !comparable(own, row)) continue;
      n++;
      const a = row.answers[lib.id];
      const b = row.answers[own.id];
      const same =
        typeof a === "string" && typeof b === "string"
          ? a === b
          : typeof a !== "string" && typeof b !== "string";
      if (same) agree++;
    }
    return `${pct(agree, n)} (${n})`;
  });
  md += `| ${suite.source.id} | ${cells.join(" | ")} |\n`;
}
md += `
Versions run: ${LIBS.map((l) => `${l.id} ${RUNNING[l.id]}`).join(", ")}.
micromatch matches through picomatch 2, which is also what fast-glob (npm)
and globby use; tinyglobby uses picomatch 4. wax refuses much of what the
others accept (\`a//b\`, \`**/**\`, \`***\`), which is most of its distance
from every column.
`;
writeFileSync(join(OUT, "README.md"), md);

for (const { suite, counts } of summary) {
  console.log(
    `${suite.source.id.padEnd(11)} harvested ${String(suite.harvested).padStart(5)}  rows ${String(suite.rows.length).padStart(5)}  agree ${counts.agree + counts["agree-error"]}  disagree ${counts.disagree}  we-reject ${counts["we-reject"]}  they-reject ${counts["they-reject"]}  (suite failures under the harness: ${suite.failures})`,
  );
}
