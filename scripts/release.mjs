#!/usr/bin/env node
// One-shot version bump across every manifest, kept in lockstep:
// Cargo.toml (workspace.package + the path-dependency pin),
// both package.json files, and Cargo.lock.
//
//   node scripts/release.mjs            # bump patch (0.0.1 -> 0.0.2)
//   node scripts/release.mjs minor      # 0.0.2 -> 0.1.0
//   node scripts/release.mjs major      # 0.1.0 -> 1.0.0
//   node scripts/release.mjs 1.2.3      # explicit target

import { readFileSync, writeFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

process.chdir(dirname(dirname(fileURLToPath(import.meta.url))));

const CARGO = "Cargo.toml";
const PKGS = ["packages/globstar/package.json", "packages/globstar-walk/package.json"];

const cargo = readFileSync(CARGO, "utf8");
const current = cargo.match(/^version = "(\d+\.\d+\.\d+)"$/m)?.[1];
if (!current) throw new Error("workspace.package version not found in Cargo.toml");

const arg = process.argv[2] ?? "patch";
let next;
if (/^\d+\.\d+\.\d+$/.test(arg)) {
  next = arg;
} else {
  const [ma, mi, pa] = current.split(".").map(Number);
  if (arg === "patch") next = `${ma}.${mi}.${pa + 1}`;
  else if (arg === "minor") next = `${ma}.${mi + 1}.0`;
  else if (arg === "major") next = `${ma + 1}.0.0`;
  else throw new Error(`expected patch | minor | major | X.Y.Z, got "${arg}"`);
}
const order = (v) => v.split(".").map(Number);
const [a, b] = [order(next), order(current)];
const newer = a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
if (newer <= 0) throw new Error(`${next} is not newer than ${current}`);

// Read and check every manifest before writing any, so an error leaves
// the tree as it was.
const line = `"version": "${current}"`;
const pkgs = PKGS.map((f) => {
  const s = readFileSync(f, "utf8");
  if (!s.includes(line)) throw new Error(`${f} is at a different version than Cargo.toml`);
  return [f, s];
});
writeFileSync(CARGO, cargo.replaceAll(`version = "${current}"`, `version = "${next}"`));
for (const [f, s] of pkgs) writeFileSync(f, s.replace(line, `"version": "${next}"`));
execSync("cargo update --workspace --quiet"); // refresh Cargo.lock

console.log(`${current} -> ${next} (Cargo.toml, Cargo.lock, ${PKGS.join(", ")})`);
console.log(`
next:
  git commit -am "release: v${next}"
  git tag v${next}
  git push origin main v${next}`);
