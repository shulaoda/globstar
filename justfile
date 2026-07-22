# Project task runner — `just <recipe>` from repo root.
# Run `just` (no args) to list recipes.

default:
    @just --list

# Install JS deps, prime Rust build cache, build memory-check binaries.
setup:
    pnpm install
    cargo build --release --workspace

# Cross-runtime correctness verification (Rust + JS × {single, multi, err}).
verify *args:
    node verify.mjs {{args}}

# Cross-runtime benchmarks → BENCHMARKS.md.
bench *args:
    node bench.mjs {{args}}

# Cross-runtime differential fuzzing (JS vs Rust on random inputs the
# corpus can't reach). `just fuzz --seeds 1-20 --count 100000` for nightly.
fuzz *args:
    cargo build --release -p difftest
    node fuzz.mjs {{args}}

# All static checks: JS format/lint + Rust fmt + clippy. CI-grade strict.
lint:
    pnpm check
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Auto-fix everything fixable: JS format/lint + Rust fmt.
fix:
    pnpm fix
    cargo fmt --all

# Rust tests (single-pattern + multi-pattern + err corpus + walker
# integration) + the JS compiler-stages golden (shared TSV fixture).
test:
    cargo test --workspace
    node packages/globstar/tests/compiler-stages.mjs

# Wipe build artifacts (Rust target + JS workspace node_modules + bench output).
clean:
    cargo clean
    rm -rf node_modules packages/*/node_modules
    rm -f BENCHMARKS.md
