// String-mode ↔ byte-mode differential for the segment engine.
//
// The segment engine matches JS strings directly (zero-copy) and must
// return exactly what byte mode returns on `toBytes(input)` — that is
// the public contract (strings are UTF-8 text). This test sweeps
// random patterns × paths, including multi-byte text, `?`-vs-bytes
// traps, and dot/class/brace shapes, asserting the two modes agree.
//
// Run: node packages/globstar/tests/string-mode.mjs [count]

import { compileMatcher } from "../src/glob.js";
import { toBytes } from "../src/utf8.js";

const COUNT = Number(process.argv[2] ?? 200000);

// mulberry32
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

const rand = rng(0xc0ffee);
const pick = (arr) => arr[(rand() * arr.length) | 0];

const PAT_TOKENS = [
  "a",
  "b",
  "cc",
  ".",
  "..",
  "/",
  "*",
  "?",
  "**/",
  "/**",
  "**",
  "{a,b}",
  "{a,}",
  "*.ts",
  "[a-c]",
  "[!a]",
  "[^b]",
  "é",
  "中",
  "{*.ts,*.tsx}",
  "n*d",
  "e?t",
  "\\*",
  "\\?",
  "x",
  ".hidden",
  "{src,lib}/",
  "?*",
];
const PATH_TOKENS = [
  "a",
  "b",
  "cc",
  ".",
  "..",
  "/",
  "x",
  "é",
  "中",
  "🙂",
  "src",
  "lib",
  ".hidden",
  "a.ts",
  "b.tsx",
  "e.t",
  "ndt",
  "née",
  "caf",
  "é",
];

function genPattern() {
  const n = 1 + ((rand() * 5) | 0);
  let s = "";
  for (let i = 0; i < n; i++) s += pick(PAT_TOKENS);
  return s;
}
function genPath() {
  const n = (rand() * 6) | 0;
  let s = "";
  for (let i = 0; i < n; i++) s += pick(PATH_TOKENS);
  return s;
}

let tried = 0;
let compiled = 0;
let bad = 0;
for (let i = 0; i < COUNT; i++) {
  const pat = genPattern();
  const dot = rand() < 0.5;
  const ci = rand() < 0.25;
  let m;
  try {
    m = compileMatcher(pat, { dot, caseInsensitive: ci });
  } catch {
    continue; // parse error — fine
  }
  compiled++;
  const path = genPath();
  tried++;

  const viaStr = m.match(path);
  const viaBytes = m.match(toBytes(path));
  if (viaStr !== viaBytes) {
    bad++;
    console.error(
      `MATCH DIVERGENCE pat=${JSON.stringify(pat)} path=${JSON.stringify(path)} dot=${dot} ci=${ci} str=${viaStr} bytes=${viaBytes}`,
    );
  }

  const dirStr = m.matchDir(path);
  const dirBytes = m.matchDir(toBytes(path));
  if (dirStr !== dirBytes) {
    bad++;
    console.error(
      `DIR DIVERGENCE pat=${JSON.stringify(pat)} dir=${JSON.stringify(path)} dot=${dot} ci=${ci} str=${dirStr} bytes=${dirBytes}`,
    );
  }
  if (bad > 20) break;
}

if (bad > 0) {
  console.error(`✗ ${bad} divergences over ${tried} cases`);
  process.exit(1);
}
console.log(`✓ string mode ≡ byte mode on ${tried} cases (${compiled} compiled patterns)`);
