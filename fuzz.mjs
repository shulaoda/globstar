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
//   node fuzz.mjs                         # 50k mixed cases, seed 1
//   node fuzz.mjs --seed 99 --count 200000
//   node fuzz.mjs --mode m                # only is_match (incl. negation)
//   node fuzz.mjs --mode d                # only match_dir (4-valued)
//   node fuzz.mjs --mode u                # only multi-pattern union
//   node fuzz.mjs --js-engine dfa         # exercise the JS DFA, not PikeVm
//   node fuzz.mjs --seeds 1-20 --count 50000   # sweep seeds (nightly)
//
// Wire format and escaping mirror tools/difftest/src/main.rs exactly.

import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { compileMatcher } from "./packages/globstar/src/matcher/glob.js";
import { GlobError } from "./packages/globstar/src/matcher/error.js";

const ROOT = dirname(fileURLToPath(import.meta.url));
process.chdir(ROOT);

// ── CLI ──────────────────────────────────────────────────────────────
function argVal(name, def) {
  const i = process.argv.indexOf(name);
  return i >= 0 && i + 1 < process.argv.length ? process.argv[i + 1] : def;
}
const COUNT = Number(argVal("--count", "50000"));
const MODE = argVal("--mode", "all"); // all | m | d | u
const JS_ENGINE = argVal("--js-engine", "pikevm"); // pikevm | dfa
const MAX_SAMPLES = Number(argVal("--max-samples", "30"));
// `--seed N` for one run, or `--seeds A-B` to sweep a range.
const SEEDS = (() => {
  const range = argVal("--seeds", null);
  if (range) {
    const [a, b] = range.split("-").map(Number);
    const out = [];
    for (let s = a; s <= b; s++) out.push(s);
    return out;
  }
  return [Number(argVal("--seed", "1"))];
})();

// Engine override threaded into compileMatcher. `undefined` = the shipped
// default (PikeVm for one-shot callers); "dfa" forces the walker's engine.
const ENGINE_OPT = JS_ENGINE === "dfa" ? "dfa" : undefined;

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
// Paths are arbitrary BYTES. `\xHH` tokens inject raw / invalid-UTF-8
// bytes; whole-char tokens inject Latin-1 and explicit UTF-8 sequences.
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
// PATTERN: encode the string as UTF-8 — identical bytes to what the JS
// parser's toBytes() sees and to what Rust's from_utf8 reconstructs.
const patternWire = (s) => escapeBytes(ENC.encode(s));
// PATH: each JS char's low byte is one raw path byte.
function pathBytes(s) {
  const a = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) a[i] = s.charCodeAt(i) & 0xff;
  return a;
}
const pathWire = (s) => escapeBytes(pathBytes(s));

// ── JS reference results ─────────────────────────────────────────────
const DIR_TOKEN = ["pruned", "descend", "match", "descend-match"];

function jsResult(c) {
  const opts = { dot: c.dot, caseInsensitive: c.ci, __engine: ENGINE_OPT };
  try {
    if (c.cmd === "m") {
      return compileMatcher(c.pat, opts).match(pathBytes(c.path)) ? "match" : "no-match";
    }
    if (c.cmd === "d") {
      return DIR_TOKEN[compileMatcher(c.pat, opts).matchDir(pathBytes(c.dir))];
    }
    // union
    return compileMatcher(c.patterns, opts).match(pathBytes(c.path)) ? "match" : "no-match";
  } catch (e) {
    return e instanceof GlobError ? "err:" + e.kind : "err:UNKNOWN(" + (e && e.message) + ")";
  }
}

function wireLine(c) {
  const f = `${c.dot ? 1 : 0}${c.ci ? 1 : 0}`;
  if (c.cmd === "m") return `m\t${f}\t${patternWire(c.pat)}\t${pathWire(c.path)}`;
  if (c.cmd === "d") return `d\t${f}\t${patternWire(c.pat)}\t${pathWire(c.dir)}`;
  // union: u <flags> <path> <pat...>
  return `u\t${f}\t${pathWire(c.path)}\t${c.patterns.map(patternWire).join("\t")}`;
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
    const cmd = MODE === "all" ? pick(["m", "m", "m", "d", "u"]) : MODE;
    const dot = chance(0.5);
    const ci = chance(0.5);
    if (cmd === "m") return { cmd, dot, ci, pat: genPattern(10), path: genPath(8) };
    if (cmd === "d") return { cmd, dot, ci, pat: stripBang(genPattern(10)), dir: genPath(8) };
    // union: 2..10 positive patterns (Glob::union rejects negation; the JS
    // union path factors positives) — large counts push the merged NFA past
    // the 64-state OrEngine-decomposition threshold on both sides.
    const k = randint(2, 10);
    const patterns = [];
    for (let i = 0; i < k; i++) patterns.push(stripBang(genPattern(6)));
    return { cmd, dot, ci, path: genPath(8), patterns };
  };
}

// ── run one seed ─────────────────────────────────────────────────────
function runSeed(seed) {
  const gen = makeGen(seed);
  const cases = Array.from({ length: COUNT }, () => gen());

  const jsResults = cases.map(jsResult);
  const input = cases.map(wireLine).join("\n") + "\n";

  const r = spawnSync("cargo", ["run", "--quiet", "--release", "-p", "difftest"], {
    input,
    encoding: "utf8",
    maxBuffer: 1 << 30,
    cwd: ROOT,
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
  return { seed, n: jsResults.length, diff, samples, jsResults };
}

// ── main ─────────────────────────────────────────────────────────────
console.log(
  `[fuzz] mode=${MODE} js-engine=${JS_ENGINE} count=${COUNT} seeds=[${SEEDS[0]}..${SEEDS[SEEDS.length - 1]}]`,
);
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

if (totalDiff > 0) {
  console.log(`\n--- divergence samples (first ${MAX_SAMPLES}) ---`);
  for (const { c, js, rust } of allSamples) {
    const desc =
      c.cmd === "u"
        ? `patterns=${JSON.stringify(c.patterns)} path=${JSON.stringify(c.path)}`
        : c.cmd === "d"
          ? `pattern=${JSON.stringify(c.pat)} dir=${JSON.stringify(c.dir)}`
          : `pattern=${JSON.stringify(c.pat)} path=${JSON.stringify(c.path)}`;
    console.log(`  [${c.cmd}] ${desc} dot=${c.dot} ci=${c.ci}  js=${js}  rust=${rust}`);
  }
  console.log(`\n✗ ${totalDiff}/${totalN} JS↔Rust divergences in ${dt}s`);
  process.exit(1);
}

console.log(`\n✓ JS and Rust agree on all ${totalN} generated cases (${dt}s)`);
