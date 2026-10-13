---
layout: home

hero:
  name: globstar
  text: One glob dialect for Rust and JavaScript
  tagline: A Rust crate and a JavaScript package that match byte for byte, with directory pruning built in.
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: Try the playground
      link: /playground
    - theme: alt
      text: Pattern syntax
      link: /guide/syntax

features:
  - title: Two runtimes, one answer
    details: The Rust crate is the canonical implementation. The JavaScript package is held to the same semantics by a shared golden corpus and a differential fuzzer that runs both.
  - title: Built for walkers
    details: match_dir tells a walker whether a directory matches, whether anything below it can, and whether everything below it does. static_prefixes says where a walk can start.
  - title: Small and dependency-free
    details: No runtime dependencies in either language. Pure string matching, no filesystem access, no node:* imports. The playground on this site runs the real package in your browser.
  - title: Written down first
    details: Every rule lives in GLOB_SPEC before it lives in code. A behavior is stable only when the shared corpus has a row for it.
---
