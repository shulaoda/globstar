---
aside: false
outline: false
---

# Playground

Type patterns on the left and paths on the right. Every path is tested twice: as a file with `match()`, and as a directory with `matchDir()`, whose six verdicts are explained in [Directory matching](./guide/directory-matching.md). The page runs the real `@globstar/core` source in your browser, so what you see is what the library does. The URL updates as you type; copy it to share a case.

<ClientOnly>
  <Playground />
</ClientOnly>

::: tip Reading the directory columns
`matches` is about the directory's own path. `may match below` is what a pattern that _selects_ files reads: when it is `no`, a walker can skip the directory. `matches all below` is what a pattern that _excludes_ files reads: when it is `yes`, a walker can skip the directory whole. It is always `no` with `dot` off, because a dot-led name below escapes every wildcard.
:::

A few things worth trying:

- `src/**` against `src` with `dot` on and off. With `dot` on, everything below `src` matches, so the verdict is `DescendAll`.
- `**/*.log` against `app.log` and against `app.log/notes.txt`. The directory matches, but a file below it need not. An exclude list must not skip it.
- `[unclosed` or `a{b,c`. Compile errors are shown with their kind.
- `*.TS` against `index.ts` with `caseInsensitive` on.
