// Segment engine ↔ PikeVM differential.
//
// The segment engine matches JS strings directly (zero-copy); PikeVM is the
// independent byte-machine reference. On the same pattern × path the two must
// agree. This sweeps random patterns × paths, including multi-byte text,
// `?`-vs-bytes traps, and dot/class/brace shapes, asserting they agree.
//
// Run: node packages/globstar/tests/string-mode.mjs [count]

import { compileMatcher } from "../src/glob.js";

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
  let seg, ref;
  try {
    seg = compileMatcher(pat, { dot, caseInsensitive: ci });
    ref = compileMatcher(pat, { dot, caseInsensitive: ci, __engine: "pikevm" });
  } catch {
    continue; // parse error — fine
  }
  compiled++;
  const path = genPath();
  tried++;

  const viaSeg = seg.match(path);
  const viaRef = ref.match(path);
  if (viaSeg !== viaRef) {
    bad++;
    console.error(
      `MATCH DIVERGENCE pat=${JSON.stringify(pat)} path=${JSON.stringify(path)} dot=${dot} ci=${ci} seg=${viaSeg} pike=${viaRef}`,
    );
  }

  const dirSeg = seg.matchDir(path);
  const dirRef = ref.matchDir(path);
  if (dirSeg !== dirRef) {
    bad++;
    console.error(
      `DIR DIVERGENCE pat=${JSON.stringify(pat)} dir=${JSON.stringify(path)} dot=${dot} ci=${ci} seg=${dirSeg} pike=${dirRef}`,
    );
  }
  if (bad > 20) break;
}

if (bad > 0) {
  console.error(`✗ ${bad} divergences over ${tried} cases`);
  process.exit(1);
}
console.log(`✓ segment ≡ pikevm on ${tried} cases (${compiled} compiled patterns)`);
