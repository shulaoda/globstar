import { defineConfig } from "vite-plus";

const ignorePatterns = [
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  "crates/**",
  "tools/**",
  "fixtures/**",
];

export default defineConfig({
  fmt: { ignorePatterns },
  lint: {
    ignorePatterns,
    options: { typeAware: true, typeCheck: true },
  },
});
