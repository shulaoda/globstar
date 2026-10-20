// The test suites the ecosystem corpus is harvested from, pinned to the
// version of the library the workspace runs (`packages/bench` for JS,
// `Cargo.lock` for Rust), and where each lives once fetched.

import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

export const SOURCES = [
  {
    id: "picomatch",
    side: "js",
    version: "4.0.7",
    tarball: "https://codeload.github.com/micromatch/picomatch/tar.gz/refs/tags/4.0.7",
    tree: "https://github.com/micromatch/picomatch/tree/4.0.7/test",
    note: "what tinyglobby matches with",
  },
  {
    id: "micromatch",
    side: "js",
    version: "4.0.8",
    tarball: "https://codeload.github.com/micromatch/micromatch/tar.gz/refs/tags/4.0.8",
    tree: "https://github.com/micromatch/micromatch/tree/4.0.8/test",
    note: "picomatch 2 inside; what fast-glob (npm) and globby match with",
  },
  {
    id: "minimatch",
    side: "js",
    version: "10.2.6",
    tarball: "https://codeload.github.com/isaacs/minimatch/tar.gz/refs/tags/v10.2.6",
    tree: "https://github.com/isaacs/minimatch/tree/v10.2.6/test",
    note: "the Bash 1.14.7 glob-test list and its additions",
  },
  {
    id: "globset",
    side: "rust",
    version: "0.4.20",
    tarball: "https://static.crates.io/crates/globset/globset-0.4.20.crate",
    tree: "https://github.com/BurntSushi/ripgrep/blob/master/crates/globset/src/glob.rs",
    note: "",
  },
  {
    id: "fast-glob",
    side: "rust",
    version: "1.1.2",
    tarball: "https://codeload.github.com/oxc-project/fast-glob/tar.gz/refs/tags/v1.1.2",
    tree: "https://github.com/oxc-project/fast-glob/blob/v1.1.2/tests/test.rs",
    note: "",
  },
  {
    id: "wax",
    side: "rust",
    version: "0.7.0",
    tarball: "https://codeload.github.com/olson-sean-k/wax/tar.gz/refs/tags/v0.7.0",
    tree: "https://github.com/olson-sean-k/wax/blob/v0.7.0/src/lib.rs",
    note: "",
  },
];

// Fetch a source once into `cacheDir/<id>-<version>` and return that path.
export async function fetchSource(source, cacheDir) {
  const dir = join(cacheDir, `${source.id}-${source.version}`);
  if (existsSync(join(dir, ".complete"))) return dir;
  mkdirSync(dir, { recursive: true });
  const res = await fetch(source.tarball);
  if (!res.ok) throw new Error(`${source.tarball}: HTTP ${res.status}`);
  const tgz = join(cacheDir, `${source.id}-${source.version}.tgz`);
  writeFileSync(tgz, Buffer.from(await res.arrayBuffer()));
  execFileSync("tar", ["xzf", tgz, "-C", dir, "--strip-components=1"], { stdio: "inherit" });
  writeFileSync(join(dir, ".complete"), "");
  return dir;
}
