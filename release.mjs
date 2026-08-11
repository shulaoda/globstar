#!/usr/bin/env node
// One-shot version bump across every manifest, kept in lockstep:
// Cargo.toml (workspace.package + the two path-dependency pins),
// both package.json files, and Cargo.lock.
//
//   node release.mjs            # bump patch (0.0.1 -> 0.0.2)
//   node release.mjs minor      # 0.0.2 -> 0.1.0
//   node release.mjs major      # 0.1.0 -> 1.0.0
//   node release.mjs 1.2.3      # explicit target

import { readFileSync, writeFileSync } from "node:fs";
import { execSync } from "node:child_process";

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
if (next === current) throw new Error(`already at ${current}`);

writeFileSync(CARGO, cargo.replaceAll(`version = "${current}"`, `version = "${next}"`));
for (const f of PKGS) {
  const s = readFileSync(f, "utf8");
  const line = `"version": "${current}"`;
  if (!s.includes(line)) throw new Error(`${f} is at a different version than Cargo.toml`);
  writeFileSync(f, s.replace(line, `"version": "${next}"`));
}
execSync("cargo update --workspace --quiet"); // refresh Cargo.lock

console.log(`${current} -> ${next} (Cargo.toml, Cargo.lock, ${PKGS.join(", ")})`);
console.log(`
next:
  git commit -am "release: v${next}"
  git tag v${next}
  git push origin main v${next}`);
