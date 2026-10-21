# Project task runner — `just <recipe>` from repo root.
# Run `just` (no args) to list recipes.

default:
    @just --list

# Install JS deps, prime Rust build cache, wire the pre-commit hook.
setup:
    pnpm install
    cargo build --release --workspace
    git config core.hooksPath .githooks

# Cross-runtime correctness verification (Rust + JS × {single, multi, dir, mdir}).
verify *args:
    node scripts/verify.mjs {{args}}

# Cross-runtime benchmarks → BENCHMARKS.md.
bench *args:
    node scripts/bench.mjs {{args}}

# Bump every manifest version in lockstep (patch | minor | major | X.Y.Z),
# then print the commit/tag/push commands that trigger the release CI.
release version="patch":
    node scripts/release.mjs {{version}}

# Cross-runtime differential fuzzing (JS vs Rust on random inputs the
# corpus can't reach; Rust built with debug checks on). `just fuzz
# --seeds 1-20 --count 100000` for nightly.
fuzz *args:
    cargo build --profile fuzz -p difftest
    node scripts/fuzz.mjs {{args}}

# All static checks: JS format/lint + Rust fmt + clippy. CI-grade strict.
lint:
    pnpm check
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Auto-fix everything fixable: JS format/lint + Rust fmt.
fix:
    pnpm fix
    cargo fmt --all

# Rust tests (single-pattern + multi-pattern corpus + walker
# integration + bounded-exhaustive matchDir properties) + JS goldens: the
# compiler-stages TSV fixture, the string-mode↔byte-mode differential
# (fixed-seed, deterministic), the JS matchDir exhaustive twin, the
# large-pattern checks, and the JS walker integration suite (temp-tree
# fixtures, twin of walk.rs). Every `packages/*/tests/*.mjs` is a suite,
# run as its own process, one at a time (some time themselves).
test:
    cargo test --workspace
    node --test --test-concurrency=1 'packages/*/tests/*.mjs'

# Regenerate fixtures/ecosystem/ from the other glob libraries' test suites
# (fetched once into node_modules/.cache/ecosystem/).
ecosystem:
    node scripts/ecosystem/generate.mjs

# Wipe build artifacts (Rust target + JS workspace node_modules). BENCHMARKS.md
# is tracked and stays.
clean:
    cargo clean
    rm -rf node_modules packages/*/node_modules
