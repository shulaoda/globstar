<script setup lang="ts">
import { compileMatcher, DirMatch, GlobError } from "@globstar/core";
import { computed, onMounted, ref, watch } from "vue";

interface Preset {
  name: string;
  patterns: string;
  paths: string;
  dot?: boolean;
}

const PRESETS: Preset[] = [
  {
    name: "Sources",
    patterns: "src/**/*.{ts,tsx}\n**/README.md",
    paths: [
      "src/index.ts",
      "src/components/App.tsx",
      "src/components/App.test.ts",
      "lib/util.js",
      "docs/README.md",
      "node_modules/x/README.md",
      "src",
      "src/components",
      "node_modules",
    ].join("\n"),
  },
  {
    name: "Ignore set",
    patterns: "**/node_modules/**\n**/.git/**\n**/*.log",
    paths: [
      "node_modules",
      "node_modules/x/index.js",
      ".git/HEAD",
      "src/app.log",
      "src/app.ts",
      "src",
      "src/app.log/notes.txt",
    ].join("\n"),
  },
  {
    name: "Dotfiles",
    patterns: "*\nsrc/**",
    paths: [".env", ".gitignore", "README.md", "src/.cache/x", "src/a.ts", "src"].join("\n"),
    dot: false,
  },
];

const patterns = ref(PRESETS[0].patterns);
const paths = ref(PRESETS[0].paths);
const dot = ref(true);
const caseInsensitive = ref(false);
const copied = ref(false);

// The non-empty lines, each with its line number.
const lines = (text: string) =>
  text
    .split("\n")
    .map((value, i) => ({ value, line: i + 1 }))
    .filter((l) => l.value.length > 0);
const patternLines = computed(() => lines(patterns.value));
// A path line that is exactly `""` is the empty path: the walk root.
const ROOT = '""';
const pathList = computed(() => lines(paths.value).map((l) => (l.value === ROOT ? "" : l.value)));

const compiled = computed(() => {
  const list = patternLines.value;
  if (list.length === 0) {
    return { matcher: null, error: "Add at least one pattern." };
  }
  try {
    const matcher = compileMatcher(
      list.map((l) => l.value),
      { dot: dot.value, caseInsensitive: caseInsensitive.value },
    );
    return { matcher, error: null };
  } catch (e) {
    if (!(e instanceof GlobError)) return { matcher: null, error: String(e) };
    // A list's error names the member by its place in the list (`index`);
    // show the line it is on instead.
    const at = e.index === undefined ? "" : `line ${list[e.index].line}: `;
    const message = e.message.replace(/^pattern \d+: /, "");
    return { matcher: null, error: `${at}${e.kind}: ${message}` };
  }
});

// `DirMatch` holds the six verdicts as numbers next to its helpers;
// invert the numeric entries to show a verdict by name.
const VERDICT_NAME: Record<number, string> = Object.fromEntries(
  Object.entries(DirMatch)
    .filter(([, v]) => typeof v === "number")
    .map(([k, v]) => [v as number, k]),
);

const rows = computed(() => {
  const m = compiled.value.matcher;
  if (m === null) return [];
  return pathList.value.map((path) => {
    const d = m.matchDir(path);
    return {
      path,
      match: m.match(path),
      verdict: VERDICT_NAME[d],
      matches: DirMatch.matches(d),
      mayBelow: DirMatch.mayMatchBelow(d),
      allBelow: DirMatch.matchesAllBelow(d),
    };
  });
});

const matched = computed(() => rows.value.filter((r) => r.match).length);

const prefixes = computed(() => {
  const m = compiled.value.matcher;
  if (m === null) return [];
  const decoder = new TextDecoder();
  return m.staticPrefixes().map((p) => decoder.decode(p));
});

function apply(preset: Preset) {
  patterns.value = preset.patterns;
  paths.value = preset.paths;
  dot.value = preset.dot ?? true;
  caseInsensitive.value = false;
}

// The whole state lives in the URL hash, so a playground link shows
// exactly what its sender saw.
interface State {
  p: string;
  f: string;
  d: boolean;
  c: boolean;
}

function toBase64Url(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromBase64Url(text: string): string {
  const binary = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

let restored = false;

onMounted(() => {
  try {
    const hash = window.location.hash.slice(1);
    if (hash.startsWith("s=")) {
      const state = JSON.parse(fromBase64Url(hash.slice(2))) as Partial<State>;
      if (typeof state.p === "string") patterns.value = state.p;
      if (typeof state.f === "string") paths.value = state.f;
      if (typeof state.d === "boolean") dot.value = state.d;
      if (typeof state.c === "boolean") caseInsensitive.value = state.c;
    }
  } catch {
    // A malformed hash is ignored; the presets stay.
  }
  restored = true;
});

watch([patterns, paths, dot, caseInsensitive], () => {
  if (!restored) return;
  const state: State = {
    p: patterns.value,
    f: paths.value,
    d: dot.value,
    c: caseInsensitive.value,
  };
  window.history.replaceState(null, "", `#s=${toBase64Url(JSON.stringify(state))}`);
});

async function copyLink() {
  try {
    await navigator.clipboard.writeText(window.location.href);
    copied.value = true;
    setTimeout(() => {
      copied.value = false;
    }, 1500);
  } catch {
    // Clipboard access can be denied; the URL bar still has the link.
  }
}
</script>

<template>
  <div class="pg">
    <div class="pg-toolbar">
      <span class="pg-muted">Examples:</span>
      <button v-for="preset in PRESETS" :key="preset.name" class="pg-chip" @click="apply(preset)">
        {{ preset.name }}
      </button>
      <span class="pg-spacer" />
      <label class="pg-opt"><input v-model="dot" type="checkbox" /> <code>dot</code></label>
      <label class="pg-opt">
        <input v-model="caseInsensitive" type="checkbox" /> <code>caseInsensitive</code>
      </label>
      <button class="pg-chip" @click="copyLink">{{ copied ? "Copied!" : "Copy link" }}</button>
    </div>

    <div class="pg-grid">
      <label class="pg-field">
        <span class="pg-title">Patterns <small>one per line, combined with OR</small></span>
        <textarea v-model="patterns" rows="8" spellcheck="false" />
      </label>
      <label class="pg-field">
        <span class="pg-title"
          >Paths
          <small
            >one per line, <code>""</code> for the walk root; each is tested as a file and as a
            directory</small
          ></span
        >
        <textarea v-model="paths" rows="8" spellcheck="false" />
      </label>
    </div>

    <p v-if="compiled.error !== null" class="pg-error">{{ compiled.error }}</p>

    <template v-else>
      <p class="pg-summary">
        <strong>{{ matched }}</strong> of {{ rows.length }} paths match. Static prefixes:
        <code v-for="(p, i) in prefixes" :key="i" class="pg-prefix">{{
          p === "" ? "(root)" : p
        }}</code>
      </p>

      <div class="pg-table-wrap">
        <table class="pg-table">
          <thead>
            <tr>
              <th>Path</th>
              <th>As a file</th>
              <th>As a directory</th>
              <th title="The directory's own path matches">matches</th>
              <th title="Something below the directory may match">may match below</th>
              <th title="Everything below the directory matches">matches all below</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(r, i) in rows" :key="i">
              <td>
                <code>{{ r.path === "" ? '"" (root)' : r.path }}</code>
              </td>
              <td :class="r.match ? 'pg-yes' : 'pg-no'">
                {{ r.match ? "✓ match" : "✗ no match" }}
              </td>
              <td>
                <code>{{ r.verdict }}</code>
              </td>
              <td :class="r.matches ? 'pg-yes' : 'pg-muted'">{{ r.matches ? "yes" : "no" }}</td>
              <td :class="r.mayBelow ? 'pg-yes' : 'pg-muted'">{{ r.mayBelow ? "yes" : "no" }}</td>
              <td :class="r.allBelow ? 'pg-yes' : 'pg-muted'">{{ r.allBelow ? "yes" : "no" }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </div>
</template>

<style scoped>
.pg {
  margin: 24px 0;
}

.pg-toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 10px;
  margin-bottom: 12px;
}

.pg-spacer {
  flex: 1;
}

.pg-chip {
  padding: 4px 12px;
  border: 1px solid var(--vp-c-divider);
  border-radius: 999px;
  background: var(--vp-c-bg-soft);
  color: var(--vp-c-text-1);
  font-size: 13px;
  line-height: 20px;
  transition:
    border-color 0.2s,
    color 0.2s;
}

.pg-chip:hover {
  border-color: var(--vp-c-brand-1);
  color: var(--vp-c-brand-1);
}

.pg-opt {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  cursor: pointer;
}

.pg-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
}

@media (max-width: 720px) {
  .pg-grid {
    grid-template-columns: 1fr;
  }
}

.pg-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.pg-title {
  font-weight: 600;
  font-size: 14px;
}

.pg-title small {
  margin-left: 6px;
  font-weight: 400;
  color: var(--vp-c-text-2);
}

textarea {
  width: 100%;
  padding: 10px 12px;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  background: var(--vp-c-bg-soft);
  color: var(--vp-c-text-1);
  font-family: var(--vp-font-family-mono);
  font-size: 13px;
  line-height: 1.6;
  resize: vertical;
  tab-size: 4;
}

textarea:focus {
  outline: none;
  border-color: var(--vp-c-brand-1);
}

.pg-error {
  margin: 16px 0;
  padding: 10px 14px;
  border-left: 4px solid var(--vp-c-danger-1);
  border-radius: 6px;
  background: var(--vp-c-danger-soft);
  font-family: var(--vp-font-family-mono);
  font-size: 13px;
  white-space: pre-wrap;
}

.pg-summary {
  margin: 16px 0 8px;
  font-size: 14px;
  color: var(--vp-c-text-2);
}

.pg-prefix {
  margin-left: 6px;
}

.pg-table-wrap {
  overflow-x: auto;
}

.pg-table {
  width: 100%;
  margin: 0;
  font-size: 13px;
}

.pg-table th,
.pg-table td {
  white-space: nowrap;
}

.pg-yes {
  color: var(--vp-c-green-1);
  font-weight: 600;
}

.pg-no {
  color: var(--vp-c-text-2);
}

.pg-muted {
  color: var(--vp-c-text-3);
}
</style>
