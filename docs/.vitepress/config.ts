import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vitepress";

const SPEC = "https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md";

export default defineConfig({
  title: "globstar",
  description:
    "One glob dialect, implemented twice: a Rust crate and a JavaScript package that match byte for byte.",
  lang: "en-US",
  cleanUrls: true,
  lastUpdated: true,

  themeConfig: {
    nav: [
      { text: "Guide", link: "/guide/getting-started", activeMatch: "/guide/" },
      { text: "API", link: "/api/javascript", activeMatch: "/api/" },
      { text: "Playground", link: "/playground" },
      { text: "Spec", link: SPEC },
    ],

    sidebar: {
      "/guide/": [
        {
          text: "Introduction",
          items: [
            { text: "Getting started", link: "/guide/getting-started" },
            { text: "Pattern syntax", link: "/guide/syntax" },
          ],
        },
        {
          text: "Semantics",
          items: [
            { text: "The ** wildcard", link: "/guide/globstar" },
            { text: "Character classes", link: "/guide/character-classes" },
            { text: "Brace expansion", link: "/guide/braces" },
            { text: "How a path is matched", link: "/guide/matching" },
            { text: "Directory matching", link: "/guide/directory-matching" },
            { text: "Multi-pattern matchers", link: "/guide/multi-pattern" },
          ],
        },
        {
          text: "Reference",
          items: [
            { text: "Errors and limits", link: "/guide/errors" },
            { text: "Differences from other tools", link: "/guide/compared" },
          ],
        },
      ],
      "/api/": [
        {
          text: "API",
          items: [
            { text: "JavaScript", link: "/api/javascript" },
            { text: "Rust", link: "/api/rust" },
          ],
        },
      ],
    },

    socialLinks: [{ icon: "github", link: "https://github.com/shulaoda/globstar" }],
    search: { provider: "local" },
    editLink: {
      pattern: "https://github.com/shulaoda/globstar/edit/main/docs/:path",
      text: "Edit this page on GitHub",
    },
    outline: [2, 3],
    footer: { message: "Released under the MIT License." },
  },

  vite: {
    plugins: [leanChunks()],
    resolve: {
      alias: {
        // The playground runs the real matcher: the package source,
        // straight from the workspace, so the docs always show the
        // behavior of the checked-out code.
        "@globstar/core": fileURLToPath(
          new URL("../../packages/globstar/src/index.js", import.meta.url),
        ),
      },
    },
  },
});

// VitePress makes each page's lean chunk (the page without its static
// HTML, which the server-rendered page already holds) by adding it to
// the bundle in `generateBundle`. Rolldown, under vite-plus, ignores that
// assignment, so every page asked for a `.lean.js` file that was never
// written. This emits them the way VitePress builds them, before its own
// hook removes the markers. Drop it once VitePress emits them itself.
function leanChunks(): Plugin {
  const staticStrip = /['"`]__VP_STATIC_START__[^]*?__VP_STATIC_END__['"`]/g;
  return {
    name: "globstar:lean-chunks",
    enforce: "pre",
    apply: "build",
    generateBundle(_options, bundle) {
      for (const chunk of Object.values(bundle)) {
        if (chunk.type !== "chunk" || !chunk.isEntry || !chunk.facadeModuleId?.endsWith(".md")) {
          continue;
        }
        const fileName = chunk.fileName.replace(/\.js$/, ".lean.js");
        if (fileName in bundle) continue;
        this.emitFile({ type: "asset", fileName, source: chunk.code.replace(staticStrip, '""') });
      }
    },
  };
}
