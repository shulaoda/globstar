# WALKER_SPEC — Filesystem Walker Built on the Glob Matcher

> This document specifies the filesystem walker that consumes the glob matcher defined in [GLOB_SPEC.md](./GLOB_SPEC.md). Pattern syntax, match semantics, and `match_dir` semantics live in the glob spec; the walker spec covers only what is layered on top: the public API, option surface, pattern preprocessing, traversal model, and error handling. **Version:** v0.3.0 (draft) **Status:** active — not yet released. **Reference implementation:** JS — `impl/packages/globstar` (`glob` / `globSync` exports). The Rust crate `impl/crates/globstar-walk` adopts a different API shape (iterator yielding `Result<DirEntry, WalkError>` per item, no async path, errors surfaced inline rather than thrown). Its behavior is governed by its own crate-level documentation, not this spec. **Normative keywords:** "MUST", "SHOULD", "MAY" follow RFC 2119.

## Table of Contents

1. [Scope](#1-scope)
2. [Public API](#2-public-api)
3. [Options](#3-options)
4. [Pattern Preprocessing](#4-pattern-preprocessing)
5. [`cwd` Resolution and Validation](#5-cwd-resolution-and-validation)
6. [Static-prefix Seeding](#6-static-prefix-seeding)
7. [Traversal Model](#7-traversal-model)
8. [Output](#8-output)
9. [Error Handling](#9-error-handling)
10. [Symlink Behavior](#10-symlink-behavior)
11. [Differences from Other Walkers](#11-differences-from-other-walkers)
12. [Appendix A: Version History](#12-appendix-a-version-history)
13. [Appendix B: Glossary](#13-appendix-b-glossary)

---

## 1. Scope

### 1.1 In scope

The walker layer adds the following on top of the glob matcher:

- A pair of public entry points returning matching file paths: a concurrent async `glob` and a sync DFS `globSync`.
- Pattern preprocessing — auto-splitting `!`-prefixed entries into the ignore set and stripping leading `/` so static prefixes resolve relative to `cwd`.
- Filesystem traversal driven by the matcher's `match_dir` and `static_prefixes` outputs.
- Output shaping — files-only, with paths relative to `cwd` or absolute.
- Loud error semantics — IO and pattern compile failures both throw (`WalkError`), no silent swallow.

### 1.2 Out of scope

- Pattern syntax, match semantics, dot-protection, character classes, brace expansion, `**` semantics, `match_dir` definition — see GLOB_SPEC.md.
- gitignore-style "negate the previous rule" semantics.
- File-content filters.
- Watching for filesystem changes.

---

## 2. Public API

### 2.1 Function signatures

```ts
export function glob(
  patterns: string | readonly string[],
  options?: GlobOptions,
): Promise<string[]>;

export function globSync(patterns: string | readonly string[], options?: GlobOptions): string[];
```

### 2.2 Choosing async vs sync

`glob` (async) is the recommended entry for any non-trivial corpus. Each discovered directory schedules its own `fs.readdir` callback immediately, letting libuv overlap syscalls across the threadpool.

`globSync` (sync DFS) is appropriate when blocking traversal is acceptable AND the corpus is small enough that I/O concurrency cannot amortize the per-await microtask cost (a few hundred files or fewer). On real Vite-style projects the async path is consistently 30–40% faster than its sync counterpart.

Both paths share the same matching, preprocessing, error model, and output shape. The only difference is how they sequence `readdir` calls.

---

## 3. Options

### 3.1 Surface

```ts
interface GlobOptions {
  cwd?: string; // default: "."
  dot?: boolean; // default: false
  caseInsensitive?: boolean; // default: false
  followSymlinks?: boolean; // default: true
  ignore?: readonly string[]; // default: []
}
```

### 3.2 Per-option semantics

#### `cwd`

The directory the patterns and `ignore` globs resolve against. Internally resolved to an absolute path at construction time, so `process.chdir` (or equivalent) after the call has NO effect on the walk. See §5 for validation.

#### `dot`

Forwarded to the matcher. The walker default is `false` (Unix "hide-dotfile" convention) — `**/*` does NOT descend into `.git/`, `.cache/`, etc. Refer to GLOB_SPEC.md §11.1 for the precise positional semantics.

This is the only option whose default differs between the matcher layer (`true`) and the walker layer (`false`). The split is intentional: dot protection is a filesystem convention, not a property of the pattern language.

#### `caseInsensitive`

Forwarded to the matcher (ASCII-only). See GLOB_SPEC.md §11.2 for the precise semantics. The walker honors the matcher's behavior on the runtime hot path; the seeding caveat is documented in §5 below.

#### `followSymlinks`

Default is **`true`**, matching `tinyglobby`'s `followSymbolicLinks` default. Vite's `import.meta.glob` relies on this (e.g. pnpm-style `node_modules/<pkg>` symlinks must be traversed for the glob to see the real package contents).

When `true`, the walker resolves symbolic-link directory entries via `fs.statSync` to determine whether the link target is a directory and should be descended. To prevent infinite descent on cyclic symlinks, the walker MUST also `fs.realpathSync` each symlink target and check the resolved path against the chain of symlink-target ancestors that lead to the current frame; if it's already in the chain, the descent is skipped (the symlink path is still emitted as a dir match if the matcher matches it — only the descent is suppressed). See §7.1 step 5 below.

When `false`, symlinks are dropped entirely: NOT emitted as files, NOT descended. This matches `fdir`'s `excludeSymlinks` mode and `tinyglobby`'s `followSymbolicLinks: false`.

#### `ignore`

An additional set of glob patterns. A path matches the walker's final predicate if and only if:

```
final_match(path) := positive.match(path) AND NOT ignore.match(path)
```

`ignore` patterns are compiled with the same `dot` / `caseInsensitive` options as the positive set. Each `ignore` entry is itself a strictly positive pattern (the array is NOT subject to the §4.2 `!`-split rule; the `!` prefix would just be treated as the matcher's whole-pattern negation per GLOB_SPEC.md §9.3).

### 3.3 Toggles deliberately NOT provided

The walker exposes 5 options. The following toggles, common in other walkers, are deliberately NOT exposed:

| Toggle                          | Reason                                                                                                                                                                                                                                                                                                                                                      |
| ------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `onlyFiles` / `onlyDirectories` | Walker is hardwired to emit files only. Directory matches are used internally for descend gating but never appear in the output (matches the `onlyFiles: true` default of fast-glob and tinyglobby).                                                                                                                                                        |
| `absolute`                      | Output is hardwired to absolute (`cwd`-joined) paths — what Vite's `import.meta.glob` always passes (`absolute: true`) when calling tinyglobby in its source. Callers wanting paths relative to `cwd` should `path.relative(cwd, p)` at their boundary; this keeps the walker single-form and avoids the surface area cost of supporting two output shapes. |
| `onError` callback              | IO errors throw (§9). Silent-swallow with an opt-in callback adds option surface for a feature that masks broken-cwd typos in the default case.                                                                                                                                                                                                             |
| `withFileTypes`                 | Output is always `string[]`. Callers that want metadata can `fs.stat` the result.                                                                                                                                                                                                                                                                           |

---

## 4. Pattern Preprocessing

### 4.1 Input shape

`patterns` MAY be a single string or an array of strings. A single string is processed as if it were a one-element array. An empty array, OR an array whose elements all reduce to ignore-only entries, results in the walker emitting an empty result without traversing.

### 4.2 `!`-prefix split (parity rule)

For each pattern in the input array, the walker counts the run of leading `!` bytes and routes by parity (matching the matcher's internal parity rule from GLOB_SPEC.md §9.5):

- count is odd → strip ALL `!`s, push the body into the ignore set;
- count is even → strip ALL `!`s, push the body into the positive set.

Examples:

| Input pattern   | `!` count | Body           | Routed to |
| --------------- | --------- | -------------- | --------- |
| `src/**/*.ts`   | 0         | `src/**/*.ts`  | positive  |
| `!**/*.test.ts` | 1         | `**/*.test.ts` | ignore    |
| `!!**/*.ts`     | 2         | `**/*.ts`      | positive  |
| `!!!**/*.ts`    | 3         | `**/*.ts`      | ignore    |

This is the recommended way to express include/exclude:

```
glob(["src/**/*.ts", "!**/*.test.ts"], { cwd })
≡  glob(["src/**/*.ts"], { cwd, ignore: ["**/*.test.ts"] })
```

Filenames that literally start with `!` MUST be escaped (`\!foo` or `[!]foo`), as documented in GLOB_SPEC.md §9.4.

### 4.3 Leading `/` strip

Each individual pattern (in BOTH the positive and ignore sets, BOTH from the input array and from `opts.ignore`) has its leading `/` run stripped before compilation:

- `/a/b/*.ts` ≡ `a/b/*.ts`;
- `//foo` ≡ `foo`.

This guarantees that the matcher's static prefixes (used for traversal seeding, §6) always come out relative to `cwd`. Patterns are scoped to `cwd`; absolute filesystem paths cannot be glob'd through the walker API — callers wanting to walk `/etc/...` MUST pass `cwd: "/etc"`.

### 4.4 Compilation

After preprocessing, the walker compiles two matchers:

- `matcher` — the OR-union of the (preprocessed) positive set;
- `ignore` — the OR-union of the (preprocessed) `opts.ignore` plus the `!`-routed negative set, or `null` when both are empty.

If the positive set is empty after preprocessing (e.g. `glob(["!foo"])` with no other inputs), the walker MUST short-circuit and return an empty result without attempting any traversal.

If matcher compilation fails, the walker MUST throw a `WalkError("InvalidPattern")` carrying the offending pattern (or the comma-joined set when multiple) and the underlying parse reason. See §9.

---

## 5. `cwd` Resolution and Validation

The walker MUST resolve `opts.cwd` to an absolute path immediately during construction (`path.resolve`). This guarantees that any subsequent change to the process working directory (`process.chdir`) has no effect on the in-flight walk.

The resolved `cwd` MUST then be validated:

- `fs.statSync(cwd)` MUST succeed; otherwise the walker throws `WalkError("Io", { path: cwd, cause })`.
- The resulting stat MUST report a directory; otherwise the walker throws `WalkError("Io", { path: cwd, cause: <"cwd is not a directory" message> })`.

A typo'd or non-existent `cwd` therefore fails loudly at the start of the walk, rather than silently producing an empty result.

**Case-insensitive seeding caveat.** The walker's static-prefix optimization (§6) seeds traversal at literal pattern prefixes using their original case. On case-insensitive filesystems (macOS APFS, Windows NTFS) where the on-disk casing differs from the pattern, the seeded subtree may be missed — the prefix `Src` may not resolve to the directory `src` even though their `is_match` results are equivalent under `caseInsensitive: true`. Workarounds: write the prefix in the actual on-disk casing, OR use an explicit class (`[Ss]rc/...`).

---

## 6. Static-prefix Seeding

The matcher exposes `static_prefixes` (GLOB_SPEC.md §13.5 implementation note, and see `engine/ops.js::computeStaticPrefixes` in the JS reference): the deepest segment-bounded literal prefix per branch of the pattern's top-level brace. Examples:

| Pattern           | Static prefixes                         |
| ----------------- | --------------------------------------- |
| `src/main.rs`     | `["src/main.rs"]` (fully literal)       |
| `src/*.ts`        | `["src"]`                               |
| `src/**/*.ts`     | `["src"]`                               |
| `**/*.ts`         | `[""]` (empty — walk from `cwd` itself) |
| `{src,test}/*.rs` | `["src", "test"]`                       |

The walker uses these to skip directories that no branch of the pattern can possibly match. For each prefix the walker:

1. Computes its absolute form: `joinAbs(cwd, prefix)`.
2. Calls `fs.statSync` on that path.
   - If the stat fails (entry missing), the prefix is silently skipped — this is NOT an error. `{a,b}/foo` where only `a/` exists must still produce results from the `a` branch.
   - If the stat succeeds, classify by file-type.
3. If the stat reports a directory:
   - Push it onto the seed-frame stack with `relative = prefix`.
   - The walker may also call `matcher.match_dir(prefix)` and short- circuit pruning if the result is `Pruned`.
4. If the stat reports a file:
   - If `matcher.match(prefix)` is true, push the file's path into the output array directly (the literal prefix IS a complete match).
   - Otherwise discard it.

The empty prefix (an empty `Uint8Array`, produced by patterns that begin with `**/` or contain no literal head) seeds a single frame rooted at `cwd` itself. cwd was already validated in §5, so no re-stat is needed.

---

## 7. Traversal Model

### 7.1 Common per-frame loop

Each frame carries a list `symlinkAncestors` of `realpath`-resolved paths of every symlink the walker descended through to reach it. Seed frames have `symlinkAncestors = []`. Both sync and async share the same per-dirent matching logic. For each `{ name, isDirectory, isSymbolicLink }` returned from a `readdir` of the frame's absolute path:

1. **Symlink drop** — if the entry is a symbolic link AND `followSymlinks` is `false`, skip the entry (NOT emitted, NOT descended). Performed before any other work.
2. Compute `childRel = parentRel === "" ? name : parentRel + "/" + name`.
3. **Ignore filter** — if `ignore !== null` AND `ignore.match(childRel)`, skip the entry. (Performed before any stat to avoid paying for resolution of ignored symlinks.)
4. Resolve `isDir`:
   - If the entry is a regular directory, `isDir = true`.
   - If the entry is a symbolic link (which by step 1 means `followSymlinks: true`), `fs.statSync` the link and use the target's `isDirectory()` result (silently treating stat failures as `false`). See §10.
   - Otherwise `isDir = false`.
5. Compute `childAbs = joinAbs(parentAbs, name)` — see §7.4 for the rationale of bypassing `path.join`.
6. Branch on `isDir`:
   - **Directory.** Walker emits files only, so the directory result of `matcher.match_dir(childRel)` is consulted only for the descend decision. If `DirMatch::should_descend(dm)` is true, push a child frame into the descend list. The child frame's `symlinkAncestors` is determined by:
     - **non-symlink dir** — copy the parent's `symlinkAncestors` unchanged;
     - **symlink-to-dir** — `fs.realpathSync(childAbs)` to get `resolved`. If `resolved ∈ frame.symlinkAncestors`, this is a cycle; skip the descent. Otherwise the child's `symlinkAncestors` is `frame.symlinkAncestors ++ [resolved]`. `fs.realpathSync` failures (broken / inaccessible link) MUST also skip the descent silently.
   - **Non-directory.** If `matcher.match(childRel)` is true, push `childAbs` into the result array. Output is always the absolute form (§8.2).

### 7.2 Sync DFS

`globSync` runs depth-first traversal:

1. Build the seed-frame stack from §6, then reverse it so the first seed pops first.
2. While the stack is non-empty: pop a frame, `fs.readdirSync`, route the dirents through the per-frame loop with `results = out` and a local `descend` array, then push descended frames onto the stack in REVERSE so the next pop is the first child (preserves per-level forward order).
3. `readdirSync` failures throw immediately; the stack and any already-pushed results are dropped.

### 7.3 Async concurrent

`glob` runs a parallel BFS bounded by the libuv threadpool:

1. Build the seed-frame stack, but do NOT order it; every seed will be submitted concurrently.
2. Initialize `pending = 0` and `failed = false`.
3. The `submit(frame)` operation increments `pending`, calls `fs.readdir(frame.absolute, { withFileTypes: true }, callback)`, and returns. Inside the callback:
   - decrement `pending`;
   - if `failed` is already `true`, return (drop the late callback);
   - if `err` is set, set `failed = true` and `reject(new WalkError("Io", { path: frame.absolute, cause: err }))`, then return;
   - otherwise, run the per-frame loop with `results = out` and a `descend` shim that calls `submit(frame)` immediately for each descended child;
   - if `pending === 0` after all the work, `resolve(out)`.
4. Submit every seed.

The "drop late callback after failure" rule MUST hold so that in-flight readdirs whose callbacks fire after the first rejection do NOT reject again, do NOT enqueue more work, and do NOT touch the output. (Promise rejection is final; the JS runtime treats subsequent `reject` / `resolve` as no-ops, but the walker SHOULD NOT do extra work either.)

### 7.4 Path joining

`joinAbs(parent, child)` is the only path-concatenation primitive used inside the walker:

```js
function joinAbs(parent, child) {
  return parent.charCodeAt(parent.length - 1) === 0x2f /* '/' */
    ? parent + child
    : parent + "/" + child;
}
```

This bypasses `path.join`'s normalization (`..`, `.`, repeated `/`) because the walker only ever appends a single dirent name to a `path.resolve`'d cwd or to a previous `joinAbs` result. None of the inputs ever contain those tokens. Measured: `path.join` is ~100 µs slower per walk on the vite-vanilla fixture than `joinAbs`.

### 7.5 Result-order guarantees

- Sync DFS yields results in **forward DFS order** per directory, consistent with the order `readdirSync` returns dirents.
- Async concurrent yields results in **callback-completion order**. Direct dirents within one frame appear in the order `readdir` returns them, but order ACROSS frames is non-deterministic (whichever readdir returns first has its results pushed first).
- Callers requiring deterministic order MUST sort the result.

---

## 8. Output

### 8.1 Always file paths

The walker emits ONLY file paths. Directory paths are NEVER part of the output, even when the pattern matches them in the matcher's sense (e.g. `src/cli` matching the directory `src/cli`). This is hardwired and not configurable, matching the `onlyFiles: true` default of the JS ecosystem.

### 8.2 Path form

Each output path is the **`cwd`-prefixed absolute form** — there is no relative-output toggle. `cwd` is itself resolved to absolute at construction (§5), so output paths are always absolute regardless of the form `cwd` was passed in.

Path separators are POSIX (`/`): the walker constructs paths from dirent names by string concatenation (§7.4), so the only separators present beyond what `path.resolve(cwd)` produced are the `/`s the walker inserted. On Windows, `path.resolve` returns the platform's preferred form, but the joined output is consistent within a single call.

Callers wanting a path relative to `cwd` should `path.relative(cwd, p)` at their boundary; this matches what Vite's `import.meta.glob` implementation does (it always passes `absolute: true` to tinyglobby and post-relativizes itself).

The output array is freshly allocated per call. The walker does NOT reuse an external buffer.

### 8.3 Result shape

```ts
glob(...): Promise<string[]>;
globSync(...): string[];
```

NOT `Promise<DirEntry[]>` — see §3.3 for the rationale.

---

## 9. Error Handling

### 9.1 Error type

```ts
class WalkError extends Error {
  readonly name: "WalkError";
  readonly kind: "InvalidPattern" | "Io";
  // For "InvalidPattern":
  readonly pattern?: string;
  readonly reason?: string;
  // For "Io":
  readonly path?: string;
  readonly cause?: Error;
}
```

### 9.2 `InvalidPattern`

Thrown synchronously during `prepare()` when any positive or `ignore` pattern fails to compile. The walker catches the matcher's `GlobError` (GLOB_SPEC.md §10) and re-throws it wrapped in `WalkError("InvalidPattern")` so callers have a single error type to match on at the walker layer.

### 9.3 `Io`

Thrown when:

- `fs.statSync(cwd)` fails during §5 cwd validation;
- the resolved `cwd` is not a directory;
- a `readdirSync` (sync path) or `fs.readdir` callback (async path) reports an error.

The async path REJECTS the returned Promise with the first such error and ignores subsequent late callbacks (§7.3). The sync path THROWS, abandoning any in-progress traversal.

The walker MUST NOT silently swallow IO errors (no `onError` callback, no log-and-continue). Rationale: silent swallow would mask the most common failure mode — a typo'd `cwd` — by producing an empty result, which is the worst possible UX. Callers that explicitly want to ignore unreadable subtrees MUST `try`/`catch` and decide what to do at their layer.

This is the walker's most visible divergence from `tinyglobby` and `fast-glob`, both of which silently suppress IO errors via `fdir`'s `suppressErrors: true` (or equivalent).

### 9.4 Stat failures during seeding

Seed-prefix `fs.statSync` failures (§6 step 2) are NOT errors — they SHOULD be silently skipped. This case is "this branch of the brace did not match anything that exists on disk", which is a legitimate "nothing to walk on this branch" outcome rather than something the user typo'd.

---

## 10. Symlink Behavior

Symbolic-link entries are surfaced by `readdir`'s `withFileTypes` result with `isSymbolicLink() === true` and `isDirectory() === false` — the dirent does not pre-resolve the link target. The walker's behavior depends on `followSymlinks`:

- `followSymlinks: false`: the symlink is dropped entirely — NOT emitted as a file, NOT descended. Matches `fdir`'s `excludeSymlinks` mode.
- `followSymlinks: true` (default): the walker calls `fs.statSync` on the symlink (NOT `lstatSync`), which follows it. If the result is a directory, the walker treats the symlink as a directory entry and descends into the target's contents. If the stat fails (broken link, permission error), `isDir` is silently set to `false` and the entry continues as a non-directory. Stat failures here do NOT throw `WalkError("Io")` — broken symlinks are too common in real trees to make them fatal, even in the loud-error default.

**Cycle detection.** When `followSymlinks: true`, the walker MUST break symlink cycles by resolving each symlink-target via `fs.realpathSync` and checking the resolved path against the chain of symlink-target ancestors that reached the current frame (see §7.1 step 6). If the resolved path is already in the chain, the descent is suppressed for that entry only — the symlink path is still emitted if `matcher.match` matches it, the rest of the walk continues. `realpathSync` failures (broken / inaccessible) silently skip the descent without throwing. Algorithm parity: `fdir`'s `isRecursive` plus its `state.symlinks` map.

Output paths preserve the symlink path as discovered through `readdir`. `fs.realpath` is called purely for cycle detection — the resolved path is NEVER substituted into the output.

---

## 11. Differences from Other Walkers

| Behavior                          | Us                                  | fast-glob                     | tinyglobby                   | globby                       | doublestar   |
| --------------------------------- | ----------------------------------- | ----------------------------- | ---------------------------- | ---------------------------- | ------------ |
| Concurrent async readdir          | ✅ (own impl)                       | ✅ (`@nodelib/fs.walk` async) | ✅ (`fdir`)                  | ✅ (uses fast-glob)          | n/a          |
| `onlyFiles` default               | hardwired `true`                    | configurable, default `true`  | configurable, default `true` | configurable, default `true` | configurable |
| IO errors throw                   | ✅ (default, no opt-out)            | ❌ (silent)                   | ❌ (silent, via `fdir`)      | ❌ (silent)                  | configurable |
| Bad-cwd throws                    | ✅ (validated upfront)              | ❌ (returns empty)            | ❌ (returns empty)           | ❌                           | varies       |
| `!`-prefix splits to ignore       | ✅ (parity-aware)                   | ✅ (one-`!` only)             | ✅ (one-`!`)                 | ✅                           | ✅           |
| Static-prefix seeding             | ✅ (per-pattern)                    | ✅                            | ✅                           | ✅                           | ❌           |
| `withFileTypes` option            | ❌ (drop entirely)                  | ✅                            | ✅                           | ✅                           | ✅           |
| `onError` callback                | ❌ (errors throw)                   | ❌                            | ❌                           | ❌                           | ✅           |
| `followSymlinks` default          | `true`                              | `false`                       | `true`                       | `false`                      | `false`      |
| Symlink cycle detection           | ✅ (`realpath` + ancestor chain)    | varies                        | ✅ (`fdir`'s `isRecursive`)  | varies                       | varies       |
| `followSymlinks: false` semantics | drop entirely (no emit, no descend) | emit-as-file                  | drop entirely                | emit-as-file                 | varies       |

The walker is opinionated about errors: any failure throws by default. Combined with hard-wired files-only output, hard-wired absolute output, and a 5-option surface, this trades configurability for predictability — fewer modes means fewer surprises.

---

## 12. Appendix A: Version History

- **v0.3.0** (2026-05-04, draft):
  - `followSymlinks` semantics aligned with `tinyglobby` / `fdir`:
    - default flipped from `false` to `true` (matches Vite's `import.meta.glob` contract — pnpm-style `node_modules/<pkg>` symlinks must be traversed by default).
    - When `false`, symlinks are now dropped entirely (NOT emitted, NOT descended). Was previously "emit as file, never descend"; the new behavior matches `fdir`'s `excludeSymlinks` mode.
    - When `true`, symlink cycles are now broken via `fs.realpathSync` + ancestor chain (§7.1 step 6, §10). Was previously "no cycle detection — caller's responsibility".
- **v0.2.0** (2026-05-03, draft):
  - `absolute` option **removed**. Output is hardwired to the `cwd`-prefixed absolute form. Rationale: matches Vite's `import.meta.glob` actual contract (the Vite source always passes `absolute: true` to tinyglobby and post-relativizes itself), and a single output form keeps the walker shape minimal. Callers wanting paths relative to `cwd` should `path.relative(cwd, p)` at their boundary.
- **v0.1.0** (2026-04-30, draft):
  - Public API split into `glob` (async concurrent) and `globSync` (sync DFS).
  - Hardwired `onlyFiles: true` output. `withFileTypes` and `DirEntry` removed from the public surface.
  - `onError` removed; all IO failures throw `WalkError("Io")` and propagate (sync throws / async rejects).
  - `cwd` validated upfront; missing or non-directory `cwd` throws `WalkError("Io")`.
  - `!`-prefix split for input arrays uses parity (matching the matcher's parser parity rule from GLOB_SPEC.md §9.5) instead of the earlier "exactly one `!`" rule.
  - Async traversal uses an own implementation directly on `node:fs.readdir`; the `@nodelib/fs.walk` and `fdir` dependencies were evaluated and dropped — same fan-out, no extra dependency.

---

## 13. Appendix B: Glossary

- **Walker.** The filesystem-traversal layer (`glob` / `globSync`) that invokes the matcher to enumerate matching file paths under `cwd`.
- **Frame.** One unit of walker work: an absolute directory path paired with its relative-to-`cwd` form. The traversal stack / scheduler holds frames.
- **Seed frame.** A frame produced from the matcher's static prefixes during initialization, before any `readdir` has been issued.
- **Auto-split.** The walker's preprocessing of a `patterns` array that routes `!`-prefixed entries to the ignore set (§4.2).
- **Static prefix.** The deepest segment-bounded literal prefix of a pattern branch (GLOB_SPEC.md §13.5 / §16). Walker uses these to skip directories no branch can match.
- **`joinAbs`.** The walker's path-concatenation primitive that bypasses `path.join`'s normalization for measurable speedup (§7.4).
- **Late callback.** An `fs.readdir` callback that fires after the walker has already rejected its returned Promise. The async traversal MUST drop these (§7.3).
