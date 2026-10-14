import { defineConfig } from "vite-plus";

const ignorePatterns = [
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  "crates/**",
  "tools/**",
  "references/**",
  "BENCHMARKS.md",
  "docs/.vitepress/cache/**",
  "docs/.vitepress/dist/**",
];

export default defineConfig({
  fmt: { ignorePatterns },
  lint: {
    ignorePatterns,
    options: { typeAware: true, typeCheck: true },
  },
});
