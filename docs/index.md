---
layout: home

hero:
  name: globstar
  text: One glob dialect for Rust and JavaScript
  tagline: A Rust crate and a JavaScript package that match byte for byte, with a directory verdict for walkers.
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: Playground
      link: /playground
    - theme: alt
      text: Pattern syntax
      link: /guide/syntax

features:
  - title: One dialect, two implementations
    details: The Rust crate is the canonical implementation. A shared corpus and a differential fuzzer hold the JavaScript package to the same answers.
  - title: Directory verdicts
    details: match_dir tells a walker whether a directory matches, whether anything below it may, and whether everything below it does. static_prefixes says where a walk can start.
  - title: No dependencies
    details: Pure string matching in both languages, with no filesystem access. The playground on this site runs the published package in the browser.
  - title: Specified
    details: Every rule is written down in GLOB_SPEC, and a behavior counts as stable once the shared corpus has a row for it that both implementations pass.
---
