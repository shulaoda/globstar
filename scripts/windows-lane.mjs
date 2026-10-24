// A local Windows lane: the Rust tests cross-built for
// x86_64-pc-windows-gnu and the JS suites on a Windows node.exe, both
// under wine in Docker. The first run builds the container
// `globstar-wine` (several minutes, about 2 GB); `docker rm -f
// globstar-wine` starts over, and WINE_CONTAINER names another prepared
// container. Not covered: real NTFS, and symlinks, which wine cannot make.
//
//   node scripts/windows-lane.mjs           # both runtimes
//   node scripts/windows-lane.mjs --rust    # the Rust crates only
//   node scripts/windows-lane.mjs --js      # the JS suites only

import { spawnSync } from "node:child_process";
import { globSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
for (const arg of args) {
  if (arg !== "--rust" && arg !== "--js") {
    console.error(`unknown argument ${arg} (use --rust, --js or nothing)`);
    process.exit(2);
  }
}
const want = (flag) => args.length === 0 || args.includes(flag);
const container = process.env.WINE_CONTAINER ?? "globstar-wine";
const NODE = "v24.15.0";
// One by one: `node --test` reads its children through pipes, which
// node.exe under wine cannot write to.
const JS_SUITES = globSync("packages/*/tests/*.mjs", { cwd: root }).sort();

// Runs `script` in the container from the repo root; the exit status.
function inContainer(script) {
  const env = "export WINEDEBUG=-all CARGO_TARGET_DIR=/opt/target-win";
  const r = spawnSync(
    "docker",
    ["exec", container, "bash", "-c", `${env}; cd '${root}'; ${script}`],
    {
      stdio: "inherit",
    },
  );
  if (r.error) throw r.error;
  return r.status ?? 1;
}

function ensureContainer() {
  const found = spawnSync("docker", ["inspect", "-f", "{{.State.Running}}", container], {
    encoding: "utf8",
  });
  if (found.error) throw new Error(`docker is not available: ${found.error.message}`);
  if (found.status === 0) {
    if (found.stdout.trim() !== "true")
      spawnSync("docker", ["start", container], { stdio: "inherit" });
    return;
  }
  console.log(`[lane] building container ${container} (once)`);
  const run = spawnSync(
    "docker",
    ["run", "-d", "--name", container, "-v", `${root}:${root}`, "rust:1", "sleep", "infinity"],
    { stdio: "inherit" },
  );
  if (run.status !== 0) process.exit(1);
  const setup = [
    "set -e",
    "export DEBIAN_FRONTEND=noninteractive",
    "dpkg --add-architecture amd64",
    "apt-get update -qq",
    "apt-get install -y -qq gcc-mingw-w64-x86-64 unzip curl >/dev/null",
    "apt-get install -y -qq wine64:amd64 >/dev/null || apt-get install -y -qq wine:amd64 >/dev/null",
    "rustup target add x86_64-pc-windows-gnu && rustup component add clippy",
    `curl -fsSL https://nodejs.org/dist/${NODE}/node-${NODE}-win-x64.zip -o /tmp/node.zip`,
    `unzip -q /tmp/node.zip -d /opt && mv /opt/node-${NODE}-win-x64 /opt/node-win`,
  ].join("\n");
  if (inContainer(setup) !== 0) {
    console.error(`[lane] setup failed; \`docker rm -f ${container}\` and try again`);
    process.exit(1);
  }
}

ensureContainer();
let failed = 0;
if (want("--rust")) {
  // Code under `cfg(windows)` is only compiled here.
  console.log("[lane] Rust: cargo clippy --target x86_64-pc-windows-gnu");
  if (
    inContainer(
      "cargo clippy -q --locked --target x86_64-pc-windows-gnu --workspace --all-targets -- -D warnings",
    ) !== 0
  ) {
    failed++;
  }
  console.log("[lane] Rust: cargo test --target x86_64-pc-windows-gnu (under wine)");
  const status = inContainer(
    "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine cargo test -q --locked " +
      "--target x86_64-pc-windows-gnu -p globstar -p globstar-walk",
  );
  if (status !== 0) failed++;
}
if (want("--js")) {
  for (const suite of JS_SUITES) {
    console.log(`[lane] JS: ${suite} (node.exe under wine)`);
    // node.exe under wine cannot write to a pipe: its streams go to files,
    // printed after.
    const status = inContainer(
      `wine /opt/node-win/node.exe ${suite} > /tmp/lane.out 2> /tmp/lane.err; s=$?; ` +
        `tr -d '\\r' < /tmp/lane.out; grep -v XDG_RUNTIME_DIR /tmp/lane.err | tr -d '\\r'; exit $s`,
    );
    if (status !== 0) failed++;
  }
}
if (failed > 0) {
  console.error(`✗ ${failed} Windows step(s) failed`);
  process.exit(1);
}
console.log("✓ the Windows lane passes");
