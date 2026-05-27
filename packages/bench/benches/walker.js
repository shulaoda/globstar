// Walker comparison across tree scales: globstar (default API)
// vs fast-glob / globby / tinyglobby. Output is consumed by `bench.mjs`.

import { bench, run, group, summary, do_not_optimize } from "mitata";
import fastGlob from "fast-glob";
import { globby } from "globby";
import { glob as tinyGlob } from "tinyglobby";
import { glob } from "@globstar/globstar";

import { mkdirSync, writeFileSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const TREES = [
  {
    name: "small",
    areas: 1,
    modules: 5,
    subs: 1,
    tests: 5,
    pkgs: 5,
    filesPerPkg: 5,
    distChunks: 10,
  },
  {
    name: "medium",
    areas: 3,
    modules: 20,
    subs: 5,
    tests: 50,
    pkgs: 50,
    filesPerPkg: 20,
    distChunks: 100,
  },
  {
    name: "large",
    areas: 5,
    modules: 50,
    subs: 10,
    tests: 200,
    pkgs: 200,
    filesPerPkg: 30,
    distChunks: 500,
  },
];
const AREAS = ["cli", "server", "client", "core", "shared"];

function buildTree(shape) {
  const root = join(tmpdir(), `glob-bench-tree-multi-js-${shape.name}`);
  if (existsSync(join(root, ".built"))) return root;
  rmSync(root, { recursive: true, force: true });
  mkdirSync(root, { recursive: true });
  const touch = (rel) => {
    const p = join(root, rel);
    mkdirSync(join(p, ".."), { recursive: true });
    writeFileSync(p, "");
  };
  for (let a = 0; a < shape.areas; a++) {
    const area = AREAS[a % AREAS.length];
    for (let m = 0; m < shape.modules; m++) {
      for (const ext of ["ts", "rs", "js", "md"]) {
        touch(`src/${area}/module_${m}.${ext}`);
        for (let s = 0; s < shape.subs; s++) {
          touch(`src/${area}/sub${s}/file_${m}.${ext}`);
        }
      }
    }
  }
  for (let i = 0; i < shape.tests; i++) {
    touch(`tests/unit/test_${i}.ts`);
    touch(`tests/integration/test_${i}.ts`);
  }
  for (let pkg = 0; pkg < shape.pkgs; pkg++) {
    for (let f = 0; f < shape.filesPerPkg; f++) {
      touch(`node_modules/pkg${pkg}/src/file${f}.ts`);
      touch(`node_modules/pkg${pkg}/dist/bundle${f}.js`);
    }
  }
  for (let i = 0; i < shape.distChunks; i++) {
    touch(`dist/chunk_${i}.js`);
    touch(`dist/chunk_${i}.js.map`);
  }
  writeFileSync(join(root, ".built"), "");
  return root;
}

const PATTERNS_SMALL = ["**/*.ts", "**/*.tsx", "**/*.js", "**/*.jsx"];
const PATTERNS_HUGE = [
  "src/**/*.ts",
  "src/**/*.tsx",
  "tests/**/*.test.ts",
  "**/*.json",
  "**/package.json",
  "components/**/*.vue",
  "scripts/**/*.sh",
  "docs/**/*.md",
  "src/**/*.spec.ts",
  "**/*.md",
];

const opts = (root) => ({ cwd: root, dot: true, onlyFiles: true });

const trees = TREES.map((s) => ({ name: s.name, root: buildTree(s) }));

for (const { name, root } of trees) {
  /** @type {Array<[string, string[]]>} */
  const sets = [
    ["multi-small", PATTERNS_SMALL],
    ["multi-huge", PATTERNS_HUGE],
  ];
  for (const [label, patterns] of sets) {
    group(`walk_${name}_${label}`, () => {
      summary(() => {
        bench("globstar", async () => do_not_optimize(await glob(patterns, opts(root))));
        bench("fast-glob", async () => do_not_optimize(await fastGlob(patterns, opts(root))));
        bench("globby", async () => do_not_optimize(await globby(patterns, opts(root))));
        bench("tinyglobby", async () => do_not_optimize(await tinyGlob(patterns, opts(root))));
      });
    });
  }
}

await run({ format: "mitata" });
