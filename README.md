# globstar

A glob library purpose-built for [Vite][vite]'s `import.meta.glob` feature.

```
crates/globstar          — Rust crate (canonical implementation)
crates/globstar-segment  — experimental segment-structured matcher (SSM)
crates/globstar-walk     — Rust filesystem walker built on globstar
packages/globstar        — JavaScript port published as @globstar/globstar
packages/globstar-segment— JS port of the SSM engine (@globstar/segment)
```

The `*-segment` crate/package implement the same dialect on a
different engine (see
[`references/theory/07-segment-matcher.md`](./references/theory/07-segment-matcher.md));
they share `globstar`'s parser and are held to the same corpus and
cross-runtime fuzzer, and BENCHMARKS.md reports both side by side.

## Why this exists

`import.meta.glob` runs glob matching on **two sides** of Vite's pipeline:

- the **Rust** side, where the build-time module graph and asset
  resolution happen;
- the **JavaScript** side, where the runtime needs the same set of
  matched paths to wire up dynamic imports.

Existing JS glob libraries (`fast-glob`, `tinyglobby`, `picomatch`) and
existing Rust crates (`globset`, `glob`, the user's own `fast-glob`)
implement subtly different dialects. Mixing two of them in one
pipeline produces "matches in dev, misses in build" bugs that are
painful to track down.

`globstar` exists to give Vite **one glob dialect** with a Rust
implementation and a JavaScript port that match byte-for-byte on every
documented edge case. The Rust crate is the canonical implementation
and the JS package is held to the same semantics via the differential
test corpus under `crates/globstar/tests/corpus/`.

## Specifications

The pattern language and walker behavior are formalized under
[`references/spec/`](./references/spec/):

- [`GLOB_SPEC.md`](./references/spec/GLOB_SPEC.md) — glob pattern
  syntax, match semantics, `match_dir`, multi-pattern union.
- [`WALKER_SPEC.md`](./references/spec/WALKER_SPEC.md) — JS filesystem
  walker (`glob` / `globSync`) layered on the matcher.

The Rust walker (`crates/globstar-walk`) ships an iterator-based API
and is governed by its crate-level documentation, not the spec above.
The pattern language spec applies to both.

## Project status

> **All packages stay on `0.0.x`.** This is deliberate.

`globstar` is being iterated for **Vite's needs**. Every release is a
work-in-progress — we adjust syntax, behavior, error model, and even
the public API surface as Vite integration uncovers new requirements
or edge cases.

- The version line stays at `0.0.x` until the spec freezes and Vite
  adopts the library in a stable release.
- Each `0.0.x` bump may carry **breaking changes** with no migration
  path. We do not maintain compatibility shims at this stage.
- The differential test corpus is the truth: a behavior is "stable"
  only when the corpus has a row for it.

**External use is not supported.** If you depend on `globstar` outside
of Vite, you accept that:

- semantics can change between any two consecutive `0.0.x` releases;
- bug reports and feature requests are triaged by Vite-relevance
  first; non-Vite issues may be deferred indefinitely;
- the published API surface (Rust crate exports, JS package exports)
  may shrink without notice.

If you want a stable glob library today, use `fast-glob` (JS),
`globset` (Rust), or `picomatch` (JS) — they have stable releases and
will not move under your feet.

[vite]: https://vitejs.dev/
