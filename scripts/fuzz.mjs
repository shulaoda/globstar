#!/usr/bin/env node
// Cross-runtime DIFFERENTIAL FUZZER — JS globstar vs Rust globstar.
//
// The fixed corpus (crates/globstar/tests/corpus/*.txt) proves the two
// runtimes agree on ~3400 hand-written rows. It says nothing about the
// infinite space of inputs nobody wrote down. This fuzzer closes that gap:
// it generates random dialect-edge (pattern, path, flags) tuples, runs the
// JS matcher, then feeds the SAME inputs to the Rust `difftest` harness over
// one stdin stream and asserts every result token agrees. A divergence is a
// real JS↔Rust drift bug, reproducible from its seed.
//
//   node scripts/fuzz.mjs                         # 50k mixed cases, seed 1
//   node scripts/fuzz.mjs --seed 99 --count 200000
//   node scripts/fuzz.mjs --mode m                # only is_match (incl. negation)
//   node scripts/fuzz.mjs --mode n                # only is_match on the NFA (chains bypassed)
//   node scripts/fuzz.mjs --mode d                # only match_dir (6-valued, incl. negation)
//   node scripts/fuzz.mjs --mode u                # only multi-pattern union
//   node scripts/fuzz.mjs --mode un               # only union is_match on the NFA
//   node scripts/fuzz.mjs --mode ud               # only union match_dir
//   node scripts/fuzz.mjs --mode s                # only static_prefixes
//   node scripts/fuzz.mjs --seeds 1-20 --count 50000   # sweep seeds (nightly)
//
// Wire format and escaping mirror tools/difftest/src/main.rs exactly. The
// Rust side runs the `fuzz` profile: release speed with `debug_assert!`
// and overflow checks on.
//
// About 5% of the cases are long (four times the tokens), for the cost
// guards and the large-graph paths.

import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { compileMatcher } from "../packages/globstar/src/glob.js";
import { GlobError } from "../packages/globstar/src/error.js";

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
process.chdir(ROOT);

// ── CLI ──────────────────────────────────────────────────────────────
function argVal(name, def) {
  const i = process.argv.indexOf(name);
  return i >= 0 && i + 1 < process.argv.length ? process.argv[i + 1] : def;
}
function usage(message) {
  console.error(`[fuzz] ${message}`);
  process.exit(2);
}
const int = (name, value) => {
  if (!/^\d+$/.test(value)) usage(`${name} needs a whole number, got ${JSON.stringify(value)}`);
  return Number(value);
};
const COUNT = int("--count", argVal("--count", "50000"));
if (COUNT === 0) usage("--count 0 tests nothing");
const MODE = argVal("--mode", "all"); // all | m | n | d | u | un | ud | s
if (!["all", "m", "n", "d", "u", "un", "ud", "s"].includes(MODE)) usage(`unknown --mode ${MODE}`);
const MAX_SAMPLES = int("--max-samples", argVal("--max-samples", "30"));
// `--seed N` for one run, or `--seeds A-B` to sweep a range.
const SEEDS = (() => {
  const range = argVal("--seeds", null);
  if (range) {
    const m = /^(\d+)-(\d+)$/.exec(range);
    if (!m || Number(m[1]) > Number(m[2]))
      usage(`--seeds needs A-B with A <= B, got ${JSON.stringify(range)}`);
    const out = [];
    for (let s = Number(m[1]); s <= Number(m[2]); s++) out.push(s);
    return out;
  }
  return [int("--seed", argVal("--seed", "1"))];
})();

// ── deterministic PRNG (mulberry32) so any failure reproduces ────────
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ── dialect-edge token alphabets ─────────────────────────────────────
// Patterns are valid UTF-8 text (they cross Rust's &str API). Bias hard
// toward the constructs most likely to drift: classes (both `!`/`^`
// negation forms, POSIX first-`]`, ranges, escapes, illegal `/`), braces
// (nested, empty branches), every `**` position, escapes, stray closers,
// and non-ASCII.
const PAT_TOKENS = [
  "a",
  "b",
  "c",
  "z",
  "A",
  "Z",
  ".",
  "-",
  "_",
  "0",
  "9",
  "/",
  "*",
  "?",
  "**",
  "**/",
  "/**",
  "/**/",
  "//",
  "[abc]",
  "[!abc]",
  "[^abc]",
  "[a-z]",
  "[A-Z]",
  "[0-9]",
  "[]a]",
  "[!]a]",
  "[/]",
  "[\\]]",
  "[\\\\]",
  "[a-]",
  "[-a]",
  "[z-a]",
  "[.]",
  "[!.]",
  "{a,b}",
  "{a,bc,def}",
  "{a}",
  "{,a}",
  "{a,}",
  "{ab,{c,d}}",
  "{a,b}{c,d}",
  // Braces that cross a separator: regrouping, pending `**/`, the
  // work budget.
  "{**,a}/",
  "{/,a,}",
  "{a/,a//}",
  "{,/}",
  "{a,/}",
  "{a/,}",
  "\\*",
  "\\?",
  "\\[",
  "\\]",
  "\\{",
  "\\}",
  "\\a",
  "\\.",
  "\\\\",
  "\\!",
  "\\/",
  "@(a)",
  "!(a)",
  "+(a)",
  "?(a)",
  "(",
  ")",
  "()",
  "|",
  "@",
  "+",
  "é",
  "ü",
  "中",
  "🎉", // valid multi-byte UTF-8 (encoded via TextEncoder)
  "!",
  "src/",
  ".txt",
  "foo",
  "node_modules",
];
// Paths are JS strings, so they reach both runtimes as valid UTF-8: the
// `\xHH` tokens are Latin-1 characters, the others multi-byte ones.
const PATH_TOKENS = [
  "a",
  "b",
  "c",
  "z",
  "A",
  "Z",
  ".",
  "..",
  "-",
  "_",
  "0",
  "9",
  "/",
  "//",
  "foo",
  "bar",
  ".hidden",
  "x.txt",
  "src",
  "a/b",
  "a/b/c",
  "",
  "é",
  "ü",
  "中",
  "🎉",
  "\xc3\xa9",
  "\xe4\xb8\xad",
  "\xff",
  "\x00",
  "\x80",
  "[abc]",
  "{a}",
  "a/",
  "/a",
  "ABC",
  "Abc",
  "node_modules",
  ".txt",
];

// ── wire codec (must match tools/difftest/src/main.rs) ───────────────
const ENC = new TextEncoder();
function escapeBytes(bytes) {
  let out = "";
  for (const b of bytes) {
    if (b === 0x5c) out += "\\\\";
    else if (b >= 0x20 && b <= 0x7e && b !== 0x09) out += String.fromCharCode(b);
    else out += "\\x" + b.toString(16).padStart(2, "0");
  }
  return out;
}
// PATTERN and PATH both travel as UTF-8 — identical bytes to what the JS
// side's UTF-8 encoder sees and to what Rust receives. The public JS
// API is string-only, so the shared input domain is UTF-8 strings; raw
// non-UTF-8 byte paths are Rust-only territory the differential can't reach.
const patternWire = (s) => escapeBytes(ENC.encode(s));
const pathWire = patternWire;

// ── JS reference results ─────────────────────────────────────────────
// Indexed by JS `DirMatch` value (Match=0, Pruned=1, Descend=2, DescendAndMatch=3,
// DescendAll=4, DescendAllAndMatch=5).
const DIR_TOKEN = [
  "match",
  "pruned",
  "descend",
  "descend-match",
  "descend-all",
  "descend-all-match",
];

// Single-pattern queries take `c.pat`, union ones `c.patterns`; `m`, `u`
// and their NFA twins `n`, `un` match `c.input`, `d` and `ud` take it as
// a directory.
const UNION = new Set(["u", "un", "ud"]);
const NFA = new Set(["n", "un"]);
const DIR = new Set(["d", "ud"]);

function jsResult(c) {
  const opts = { dot: c.dot, caseInsensitive: c.ci, __engine: NFA.has(c.cmd) ? "nfa" : undefined };
  try {
    if (c.cmd === "s") {
      const px = compileMatcher(c.patterns, opts).staticPrefixes().map(escapeBytes).sort();
      return "px:" + px.join(",");
    }
    const m = compileMatcher(UNION.has(c.cmd) ? c.patterns : c.pat, opts);
    if (DIR.has(c.cmd)) return DIR_TOKEN[m.matchDir(c.input)];
    return m.match(c.input) ? "match" : "no-match";
  } catch (e) {
    return e instanceof GlobError ? "err:" + e.kind : "err:UNKNOWN(" + (e && e.message) + ")";
  }
}

function wireLine(c) {
  const f = `${c.dot ? 1 : 0}${c.ci ? 1 : 0}`;
  if (c.cmd === "s") return `s\t${f}\t${c.patterns.map(patternWire).join("\t")}`;
  // union: <cmd> <flags> <input> <pat...>
  if (UNION.has(c.cmd)) {
    return `${c.cmd}\t${f}\t${pathWire(c.input)}\t${c.patterns.map(patternWire).join("\t")}`;
  }
  return `${c.cmd}\t${f}\t${patternWire(c.pat)}\t${pathWire(c.input)}`;
}

// ── generators ───────────────────────────────────────────────────────
function makeGen(seed) {
  const rnd = rng(seed);
  const pick = (arr) => arr[Math.floor(rnd() * arr.length)];
  const chance = (p) => rnd() < p;
  const randint = (lo, hi) => lo + Math.floor(rnd() * (hi - lo + 1));

  const genPattern = (maxTok) => {
    let s = "";
    const n = randint(1, maxTok);
    for (let i = 0; i < n; i++) s += pick(PAT_TOKENS);
    if (chance(0.3)) s = pick(["!", "!!", "/", ""]) + s; // negation / root bias
    return s;
  };
  const stripBang = (s) => {
    const t = s.replace(/^!+/, "");
    return t.length ? t : "a";
  };
  const genPath = (maxTok) => {
    let s = "";
    const n = randint(0, maxTok);
    for (let i = 0; i < n; i++) s += pick(PATH_TOKENS);
    return s;
  };

  return () => {
    const cmd = MODE === "all" ? pick(["m", "m", "m", "n", "d", "d", "u", "un", "ud", "s"]) : MODE;
    const dot = chance(0.5);
    const ci = chance(0.5);
    const scale = chance(0.05) ? 4 : 1;
    if (cmd === "s") {
      // Single patterns keep negation; unions strip it (Rust rejects it).
      const k = chance(0.5) ? 1 : randint(2, 5);
      const patterns =
        k === 1 ? [genPattern(10)] : Array.from({ length: k }, () => stripBang(genPattern(6)));
      return { cmd, dot, ci, patterns };
    }
    const input = genPath(8 * scale);
    if (!UNION.has(cmd)) return { cmd, dot, ci, pat: genPattern(10 * scale), input };
    // union: 2..10 patterns stripped of `!` — negated members are the one
    // documented API divergence (Rust rejects NegatedInUnion, JS accepts;
    // GLOB_SPEC §10), so raw equality can't compare them.
    const k = randint(2, 10);
    const patterns = [];
    for (let i = 0; i < k; i++) patterns.push(stripBang(genPattern(6 * scale)));
    return { cmd, dot, ci, input, patterns };
  };
}

// ── run one seed ─────────────────────────────────────────────────────
function runSeed(seed) {
  const gen = makeGen(seed);
  const cases = Array.from({ length: COUNT }, () => gen());

  const jsResults = cases.map(jsResult);
  const input = cases.map(wireLine).join("\n") + "\n";

  const r = spawnSync("cargo", ["run", "--quiet", "--profile", "fuzz", "-p", "difftest"], {
    input,
    encoding: "utf8",
    maxBuffer: 1 << 30,
    cwd: ROOT,
    env: process.env,
  });
  if (r.status !== 0) {
    console.error(`[fuzz] rust harness failed (status=${r.status})`);
    console.error(r.stderr);
    process.exit(2);
  }
  const rustResults = r.stdout.split("\n").filter((x) => x.length > 0);

  if (rustResults.length !== jsResults.length) {
    console.error(
      `[fuzz] COUNT MISMATCH seed=${seed} js=${jsResults.length} rust=${rustResults.length} — protocol bug, not a real divergence`,
    );
    process.exit(2);
  }

  let diff = 0;
  const samples = [];
  for (let i = 0; i < jsResults.length; i++) {
    if (jsResults[i] !== rustResults[i]) {
      diff++;
      if (samples.length < MAX_SAMPLES)
        samples.push({ c: cases[i], js: jsResults[i], rust: rustResults[i] });
    }
  }
  return { seed, n: jsResults.length, diff, samples };
}

// ── main ─────────────────────────────────────────────────────────────
console.log(`[fuzz] mode=${MODE} count=${COUNT} seeds=[${SEEDS[0]}..${SEEDS[SEEDS.length - 1]}]`);
const t0 = Date.now();
let totalDiff = 0;
let totalN = 0;
const allSamples = [];
for (const seed of SEEDS) {
  const res = runSeed(seed);
  totalDiff += res.diff;
  totalN += res.n;
  console.log(`  seed=${res.seed} n=${res.n} divergences=${res.diff}`);
  for (const s of res.samples) if (allSamples.length < MAX_SAMPLES) allSamples.push(s);
}
const dt = ((Date.now() - t0) / 1000).toFixed(1);
if (totalN === 0) usage("no case ran");

if (totalDiff > 0) {
  console.log(`\n--- divergence samples (first ${MAX_SAMPLES}) ---`);
  for (const { c, js, rust } of allSamples) {
    const what =
      c.cmd === "s" ? "" : ` ${DIR.has(c.cmd) ? "dir" : "path"}=${JSON.stringify(c.input)}`;
    const desc = `${c.patterns ? `patterns=${JSON.stringify(c.patterns)}` : `pattern=${JSON.stringify(c.pat)}`}${what}`;
    console.log(`  [${c.cmd}] ${desc} dot=${c.dot} ci=${c.ci}  js=${js}  rust=${rust}`);
  }
  console.log(`\n✗ ${totalDiff}/${totalN} JS↔Rust divergences in ${dt}s`);
  process.exit(1);
}

console.log(`\n✓ JS and Rust agree on all ${totalN} generated cases (${dt}s)`);
