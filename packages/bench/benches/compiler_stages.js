import { bench, group, run } from "mitata";
import { parse } from "../../globstar/src/matcher/parser.js";
import { lower } from "../../globstar/src/matcher/engine/ops.js";

const cases = [
  ["literal", "src/main.rs"],
  ["wildcard", "src/*.ts"],
  ["globstar", "src/**/*.ts"],
  ["brace", "**/*.{ts,tsx,js,jsx}"],
  ["nested-brace", "{{a,{b,{c,d}}},e}/**/*.rs"],
  ["separator-distribution", "{**,src/**}/mod.{rs,toml}"],
  ["star-run", "src/********************************.rs"],
];

group("stage: parse", () => {
  for (const [name, pattern] of cases) bench(name, () => parse(pattern));
});

const parsed = cases.map(([name, pattern]) => [name, parse(pattern).body]);
group("stage: lower", () => {
  for (const [name, body] of parsed) bench(name, () => lower(body, false));
});

await run();
