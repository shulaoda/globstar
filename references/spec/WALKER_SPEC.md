# WALKER_SPEC — Filesystem Walker Built on the Glob Matcher

This document specifies the filesystem walker that consumes the glob matcher defined in [GLOB_SPEC.md](./GLOB_SPEC.md). Pattern syntax, match semantics, and `match_dir` semantics live in the glob spec; the walker spec covers only what is layered on top: the public API, option surface, pattern preprocessing, traversal model, and error handling.

**Status.** Active; released as 0.0.x and may still change.

**Reference implementation.** JS — `packages/globstar-walk` (`glob` / `globSync` exports). The Rust crate `crates/globstar-walk` adopts a different API shape (iterator yielding `Result<DirEntry, WalkError>` per item, no async path, errors surfaced inline rather than thrown). It follows this spec where its documentation says so, and its own documentation otherwise.

**Conventions.** "MUST", "SHOULD", "MAY" follow RFC 2119.

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
12. [Appendix A: Glossary](#12-appendix-a-glossary)

---

## 1. Scope

### 1.1 In scope

The walker layer adds the following on top of the glob matcher:

- A pair of public entry points returning matching file paths: a concurrent async `glob` and a sync DFS `globSync`.
- Pattern preprocessing — auto-splitting `!`-prefixed entries into the ignore set and stripping leading `/` so static prefixes resolve relative to `cwd`.
- Filesystem traversal driven by the matcher's `match_dir` and `static_prefixes` outputs.
- Output shaping — files-only, absolute paths joined from `cwd` (§8).
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
export function glob(patterns: string | Iterable<string>, options?: GlobOptions): Promise<string[]>;

export function globSync(patterns: string | Iterable<string>, options?: GlobOptions): string[];
```

### 2.2 Choosing async vs sync

`glob` (async) reads directories concurrently: each discovered directory schedules its own `fs.readdir` at once, and libuv overlaps the reads on its threadpool. It does not block the event loop, and it gains most when reads are slow — a cold cache, a network or slow disk, macOS's `readdir`. It can hold more directory listings in memory at once than the sync walk does.

`globSync` (sync DFS) reads one directory at a time and pays no promise or callback cost per directory. Use it when blocking is acceptable; with a warm cache it can be the faster of the two.

Which one is faster depends on the platform, the cache and the tree, so measure where it matters. Two warm-cache runs of Vite's `packages/vite` `src/**/*.ts` (962 files): macOS (APFS) sync 7.2 ms, async 4.4 ms; Linux (tmpfs) sync 1.9 ms, async 3.0 ms.

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
  ignore?: string | readonly string[]; // default: []; bare string ≡ one-element array
}
```

An option key explicitly present with the value `undefined` means "use the default" (TypeScript optional-property semantics) — it MUST NOT clobber the default the way a naive object spread would. Only own keys are read, so a key on `Object.prototype` changes nothing. A `TypeError` with a clear message is thrown by `globSync`, and rejects the Promise of `glob`, when:

- the options value is not an object (`null` and `undefined` mean "all defaults");
- a key is not one of the five above (a typo, or another tool's name such as `caseSensitiveMatch`);
- `dot`, `caseInsensitive` or `followSymlinks` is not a boolean (`"false"` would read as true);
- `cwd` is not a string, or `ignore` is neither a string nor an array of strings.

### 3.2 Per-option semantics

#### `cwd`

The directory the patterns and `ignore` globs resolve against. Internally resolved to an absolute path at construction time, so `process.chdir` (or equivalent) after the call has NO effect on the walk. See §5 for validation.

#### `dot`

Forwarded to the matcher. The walker default is `false` (Unix "hide-dotfile" convention) — `**/*` does NOT descend into `.git/`, `.cache/`, etc. Refer to GLOB_SPEC.md §3.1 and §12 for the precise positional semantics.

This is the only option whose default differs between the matcher layer (`true`) and the walker layer (`false`). The split is intentional: dot protection is a filesystem convention, not a property of the pattern language.

#### `caseInsensitive`

Forwarded to the matcher (ASCII-only). See GLOB_SPEC.md §13 for the precise semantics. The walker honors the matcher's behavior on the runtime hot path; the seeding caveat is documented in §5 below.

#### `followSymlinks`

Default is **`true`**, matching `tinyglobby`'s `followSymbolicLinks` default. Vite's `import.meta.glob` relies on this (e.g. pnpm-style `node_modules/<pkg>` symlinks must be traversed for the glob to see the real package contents).

When `true`, the walker resolves symbolic-link directory entries via `fs.statSync` to determine whether the link target is a directory and should be descended. To prevent infinite descent on cyclic symlinks, the walker MUST also remember, along each way down, the folders it reached through a link, and skip the descent through a link whose target is one of them. The link names a directory, so it is never emitted itself (the output is files only, §8.1); the rest of the walk goes on. See §7.1 step 6 and §10 below.

When `false`, symlinks are dropped entirely: NOT emitted as files, NOT descended. This matches `fdir`'s `excludeSymlinks` mode and `tinyglobby`'s `followSymbolicLinks: false`.

#### `ignore`

An additional set of glob patterns. A path matches the walker's final predicate if and only if:

```
final_match(path) := positive.match(path)
                     AND NOT ignore.match(path)
                     AND NOT ignore.match(d) for every folder d above path, below cwd
                     AND NOT ignore.match(d ++ "/") for every such d
```

An ignored folder hides its whole subtree: `glob("**/*.js", { ignore: ["dist"] })` drops `dist/a.js`, though `dist` alone names no `.js` file.

`ignore` patterns are compiled with the same `dot` / `caseInsensitive` options as the positive set. An entry that ends in `/` (also after §4.3 normalization, `d/.` ≡ `d/`) matches folders only: a folder is asked about with its `/`, so `node_modules/` prunes the folder `node_modules` and everything below it, never a file of that name; like every pattern it is anchored at `cwd` (`**/node_modules/` for any depth). Every `ignore` entry is asked about a folder with its `/`, so one that ends in `/**` prunes the folder too (`**/node_modules/**` never reads `node_modules`), under either `dot`: a dot-led name below it that a positive pattern names is then not found, as fast-glob and tinyglobby do not find it. A positive pattern that ends in `/` matches folders only too, so it selects no file (§8.1). Each `ignore` entry is itself a strictly positive pattern: the array is NOT subject to the §4.2 `!`-split rule, and an entry that starts with `!` (after §4.3 normalization) is refused with `WalkError("InvalidPattern")`. As the matcher's whole-pattern negation (GLOB_SPEC.md §11.1) it would ignore everything except what it names, which is never what a caller writing `!src/**` into an ignore list means; escape a name that starts with `!` as `\!`.

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

`patterns` MAY be a single string or an iterable of strings (an array, a `Set`, …), read once. A single string is processed as if it were a one-element array. An empty array, OR an array whose elements all reduce to ignore-only entries, results in the walker emitting an empty result without traversing. `cwd` is still checked and the `ignore` patterns still compile first, so their errors surface (§9).

### 4.2 `!`-prefix split (parity rule)

For each pattern in the input array — AFTER the §4.3 normalization, so a leading `/` cannot shield a `!` from the split (`/!x` ≡ `!x`) — the walker counts the run of leading `!` bytes and routes by parity (matching the matcher's internal parity rule from GLOB_SPEC.md §11.1):

- count is odd → strip ALL `!`s, push the body into the ignore set;
- count is even → strip ALL `!`s, push the body into the positive set.

The body is normalized again after the strip (`!/x` exposes a fresh leading `/`), and a `!` run that this exposes counts too, so the count covers every `!` the leading `/`s, `.` segments and `!`s hide: `!/!x` ≡ `!!x` is the positive `x`.

Examples:

| Input pattern   | `!` count | Body           | Routed to |
| --------------- | --------- | -------------- | --------- |
| `src/**/*.ts`   | 0         | `src/**/*.ts`  | positive  |
| `!**/*.test.ts` | 1         | `**/*.test.ts` | ignore    |
| `!!**/*.ts`     | 2         | `**/*.ts`      | positive  |
| `!!!**/*.ts`    | 3         | `**/*.ts`      | ignore    |
| `!/!x`          | 2         | `x`            | positive  |

This is the recommended way to express include/exclude:

```
glob(["src/**/*.ts", "!**/*.test.ts"], { cwd })
≡  glob(["src/**/*.ts"], { cwd, ignore: ["**/*.test.ts"] })
```

Filenames that literally start with `!` MUST be escaped (`\!foo` or `[\!]foo`), as documented in GLOB_SPEC.md §11.3.

### 4.3 Pattern normalization

Each individual pattern (in BOTH the positive and ignore sets, BOTH from the input array and from `opts.ignore`) is normalized before compilation, so patterns are spelled the way walked relative paths are spelled:

- the leading `/` run is stripped: `/a/b/*.ts` ≡ `a/b/*.ts`, `//foo` ≡ `foo`;
- `//` runs collapse to a single `/`: `a//b/*` ≡ `a/b/*`;
- plain `.` segments are dropped (`readdir` never yields `.`): `./src/**` ≡ `src/**`, `a/./b` ≡ `a/b`. A last `.` names the folder before it, so it becomes a trailing `/` instead: `d/.` ≡ `d/`, and `*/.` matches no file (dropping it would make `*/.` mean `*`);
- a pattern that normalizes to nothing (`.`, `./`) matches nothing and is dropped.

Escaped forms (`\.`) are untouched; `/` in a pattern is always structural (`\/` is a parse error, GLOB_SPEC §4.2), so the textual rewrite is safe. Without this, `./` and `//` spellings would reach the static prefixes and the emitted absolute paths, and the same file would come out spelled differently depending on how the pattern was written.

The leading-`/` strip guarantees that the matcher's static prefixes (used for traversal seeding, §6) always come out relative to `cwd` (a brace branch that starts with `/` still gives a prefix like `/b`, which §6 drops). Patterns are scoped to `cwd`; absolute filesystem paths cannot be glob'd through the walker API — callers wanting to walk `/etc/...` MUST pass `cwd: "/etc"`.

### 4.4 Compilation

After preprocessing, the walker compiles two matchers (the Rust crate, which yields folders, adds a third for the positive patterns that end in `/`):

- `matcher` — the OR-union of the (preprocessed) positive set;
- `ignore` — the OR-union of the (preprocessed) `opts.ignore` plus the `!`-routed negative set, or `null` when both are empty;

If the positive set is empty after preprocessing (e.g. `glob(["!foo"])` with no other inputs), the walker MUST short-circuit and return an empty result without attempting any traversal.

If matcher compilation fails, the walker MUST throw a `WalkError("InvalidPattern")` naming the member the matcher failed in, as the caller wrote it (`!src/[z-a]`, not the body `src/[z-a]`), and the underlying reason. The matcher reports that member (GLOB_SPEC §6), so nothing is compiled twice. When the members together ran past the expansion budget they share (GLOB_SPEC §6.2), the member named is the one it ran out in, and the reason says the patterns share the limit. An error no one member explains (the union's compile work, `TooComplex`) names the list, cut after three: `["a", "b", "c"] and 2 more`. When preprocessing changed the member, the reason names the text its byte offsets count in: `invalid character class range … at byte 5 … (in 'src/[z-a]', as the walker reads it)`. See §9.

---

## 5. `cwd` Resolution and Validation

The walker MUST resolve `opts.cwd` to an absolute path immediately during construction (`path.resolve`). This guarantees that any subsequent change to the process working directory (`process.chdir`) has no effect on the in-flight walk. `path.resolve` works on the text: the empty `cwd` is the process working directory, `.` and `..` parts are taken out without asking the filesystem (`jump/..` is the folder holding `jump`, even when `jump` is a symlink), and no symlink is resolved. The Rust walker resolves its base the same way, so one base string names one folder in both.

The resolved `cwd` MUST then be validated:

- `fs.statSync(cwd)` MUST succeed; otherwise the walker throws `WalkError("Io", { path: cwd, cause })`.
- The resulting stat MUST report a directory; otherwise the walker throws `WalkError("Io", { path: cwd, cause: <"cwd is not a directory" message> })`.

A typo'd or non-existent `cwd` therefore fails loudly at the start of the walk, rather than silently producing an empty result.

**Case-insensitive seeding caveat.** The walker's static-prefix optimization (§6) seeds traversal at literal pattern prefixes using their original case, even under `caseInsensitive: true`. On a case-SENSITIVE filesystem (Linux ext4, case-sensitive APFS) where the on-disk casing differs from the pattern, the seed does not resolve and the whole subtree is silently skipped — `glob("src/*.rs", { caseInsensitive: true })` yields nothing when the directory is named `SRC`, even though `compileMatcher("src/*.rs", { caseInsensitive: true }).match("SRC/a.rs")` is true. On case-insensitive filesystems (default macOS APFS, Windows NTFS) the seed resolves through the filesystem's own folding and the walk works; emitted paths carry the pattern's casing rather than the on-disk casing. Workarounds: write the prefix in the actual on-disk casing, OR use an explicit class (`[Ss]rc/...`), which keeps the prefix non-literal and forces a root walk.

The same filesystem folding cuts the OTHER way under `caseInsensitive: false` on a case-insensitive filesystem: a seed spelled `src` resolves to an on-disk `SRC`, so a literal-prefixed pattern emits paths (in the pattern's spelling) that the matcher itself would reject against the on-disk spelling — the option is effectively voided for the seeded prefix — and case-variant brace prefixes (`{src,SRC,Src}/*`) emit ONE file once per resolving spelling. This is inherent to spelling-based seeds on a folding filesystem (the same trade fast-glob makes); dedup by inode is not a fix, because different spellings legitimately select different pattern branches. Callers needing exact-case walking on such filesystems should use a class-headed prefix to force a root walk. So there an `ignore` entry does not hold against a pattern that spells its literal head in another case: `.GIT/config` reaches `.git/config` past `**/.git/**`, and `.ENV` reaches `.env` past `.env`. A caller whose ignore list must hold against patterns from others (a config file, a cloned repository, a user) walks such a filesystem with `caseInsensitive: true`, which folds the ignore set as the filesystem folds names (node-glob's default on macOS and Windows).

---

## 6. Static-prefix Seeding

The matcher exposes `static_prefixes` (GLOB_SPEC.md §17, and see `computeStaticPrefixes` in `packages/globstar/src/engine/segment/facts.js`): for each expansion of the pattern's braces, its literal head up to its last whole segment, with heads a shorter one covers removed. For a union these are its members' heads. Examples:

| Pattern           | Static prefixes                         |
| ----------------- | --------------------------------------- |
| `src/main.rs`     | `["src/main.rs"]` (fully literal)       |
| `src/*.ts`        | `["src"]`                               |
| `src/**/*.ts`     | `["src"]`                               |
| `**/*.ts`         | `[""]` (empty — walk from `cwd` itself) |
| `{src,test}/*.rs` | `["src", "test"]`                       |
| `{src,lib}/main.rs` | `["lib/main.rs", "src/main.rs"]`      |

The walker uses these to skip directories that no branch of the pattern can possibly match. Seeding MUST be observationally equivalent to a root walk — a seed may only skip work, never change results. So a seed is reached the way a root walk reaches it, part by part from `cwd`, and each part is judged by the walk's own per-entry rule (§7.1) as its folder's only entry, with its `lstatSync` standing in for the dirent. Every rule of the walk then holds for seeds by construction: ignore entries and folder-only entries, the covered-folder prune, special files, `followSymlinks`, the cycle chain, broken links. For each prefix the walker:

1. **Guards.** Skip the prefix outright when any of its segments is `..`, `.` or empty (split on every platform separator — on Windows a literal `\` is a separator too): `readdir` yields none of them, so no root walk reaches such a path, and a joined `..` would escape `cwd`. A brace branch that starts with `/` gives one: `{a,/b}/*` has the prefixes `a` and `/b`, and `/b` is dropped.
2. **Walks the parts.** For each part, from `cwd` down:
   - A part whose spelling could open another entry must be in its folder's listing as written (one `readdir` per such part), or the seed is dropped: a root walk, comparing names as listed, never matches such a spelling. That is a non-ASCII name — a file system that ignores Unicode normalization (APFS) opens an NFC spelling of a name stored NFD — and, on Windows, a name Win32 rewrites: one ending in `.` or a space, a device name (`NUL`, `com1`, `con.txt`), one with a `:` (a drive, a stream; `C:/proj/src/*.ts` is never below `cwd`, so it matches nothing, as on other platforms).
   - `lstatSync` the part. If nothing can be there (§9.4: ENOENT, ENOTDIR, ELOOP, ENAMETOOLONG, EINVAL, a NUL in the name), the seed is silently dropped — this is NOT an error: `{a,b}/foo` where only `a/` exists must still produce results from the `a` branch. Any other failure (EACCES, …) is thrown, as the walk would (§9.3).
   - Judge the entry with §7.1. A part before the last must give a child frame, which the next part continues from; otherwise the seed is dropped, as a root walk would not descend there. The last part's result is the seed's: a matching file goes to the output, a folder worth walking to the seed frames.

Seed frames and seed file emissions each preserve the (deduplicated, sorted) prefix order; file-prefix emissions drain before directory walking begins. Order beyond that is unspecified (§7.5).

The empty prefix (an empty `Uint8Array`, produced by patterns that begin with `**/` or contain no literal head) seeds a single frame rooted at `cwd` itself. cwd was already validated in §5, so no re-stat is needed.

---

## 7. Traversal Model

### 7.1 Common per-frame loop

Each frame carries a list `links` of the symlinks the walker descended through to reach it, each with the identity of its target (§10). Seed frames carry the links on their prefix, gathered as §6 step 2 walks it; the frame for `cwd` itself starts empty. Both sync and async share the same per-dirent matching logic. A child's paths are its folder's heads and its name — `childRel = relHead + name` with `relHead = parentRel === "" ? "" : parentRel + "/"`, and `childAbs = withSep(parentAbs) + name` (§7.4) — and the heads are made once per folder. For each `{ name, isDirectory, isSymbolicLink }` returned from a `readdir` of the frame's absolute path:

1. **Symlink drop** — if the entry is a symbolic link AND `followSymlinks` is `false`, skip the entry (NOT emitted, NOT descended). Performed before any other work.
2. A name that came back with U+FFFD and does not exist under that spelling is skipped: it was not valid UTF-8 (Linux allows any bytes), so no string names the path, and no folder can be read through it. The Rust walker, which sees the bytes, skips a name that is not valid UTF-8 the same way: it is neither yielded nor read as a folder.
3. Compute `childRel`. **Ignore filter** — if `ignore !== null` AND `ignore.match(childRel)`, skip the entry. (Performed before any stat to avoid paying for resolution of ignored symlinks.)
4. **Type the entry.** A regular entry has its own type. A symbolic link (which by step 1 means `followSymlinks: true`) takes its target's: `fs.statSync` the link; when that fails (dangling, looping) the entry stays a link, a non-directory. See §10.
5. **Special files** — a FIFO, a socket or a device (neither a directory nor a file nor a link) is skipped (§8.1).
6. Branch on the type:
   - **Non-directory.** If `matcher.match(childRel)` is true, push `childAbs` into the result array. Output is always the absolute form (§8.2).
   - **Directory.** The walker emits files only, so `matcher.match_dir(childRel)` is consulted for the descend decision alone. The folder is skipped when `ignore.match(childRel + "/")` (§3.2: asked with its `/`, under either `dot`), when `may_match_below()` is false, or when the ignore set covers the whole subtree (`ignore.match_dir(childRel).matches_all_below()`). The last check lets an entry like `**/node_modules/**/*`, which the `/` check cannot catch, skip `node_modules` whole; it only applies under `dot: true`, since under `dot: false` a `**` never covers the dot-led names below (GLOB_SPEC §15.5) and a positive pattern may still name one. Otherwise push a child frame into the descend list, whose `links` is:
     - **non-symlink dir** — the parent's `links`, unchanged;
     - **symlink-to-dir** — the parent's `links` plus this link, unless its target is the target of one of them already (§10): then this is a cycle, and the descent is skipped.

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

A child's absolute path is `withSep(parent) + child`, the only path concatenation inside the walker; a folder's `withSep(parent)` is made once for all its entries:

```js
function withSep(parent) {
  const last = parent.charCodeAt(parent.length - 1);
  if (last === 0x2f /* '/' */) return parent;
  if (!WINDOWS) return parent + "/";
  if (last === 0x5c /* '\\' */) return parent; // a drive root, `C:\`
  return parent + (parent.startsWith("\\\\?\\") ? "\\" : "/");
}
```

This bypasses `path.join`'s normalization (`..`, `.`, repeated `/`) because the walker only ever appends a single dirent name to a `path.resolve`'d cwd or to a previous child path — and, for a seed, one part of a static prefix at a time (§6 step 1 drops `..`, `.` and empty parts). §4.3's normalization (leading-`/` strip, `//` collapse, `.`-segment removal) plus that guard are what make this premise hold for the seed join; without them, pattern spellings would reach the output paths. Measured: `path.join` is ~100 µs slower per walk than this concatenation on a small Vite project tree.

### 7.5 Result-order guarantees

- Sync DFS yields results in **forward DFS order** per directory, consistent with the order `readdirSync` returns dirents.
- Async concurrent yields results in **callback-completion order**. Direct dirents within one frame appear in the order `readdir` returns them, but order ACROSS frames is non-deterministic (whichever readdir returns first has its results pushed first).
- Callers requiring deterministic order MUST sort the result.

---

## 8. Output

### 8.1 Always file paths

The walker emits ONLY file paths. Directory paths are NEVER part of the output, even when the pattern matches them in the matcher's sense (e.g. `src/cli` matching the directory `src/cli`). This is hardwired and not configurable, matching the `onlyFiles: true` default of the JS ecosystem. FIFOs, sockets and devices are no files to read (a reader would block on a FIFO) and are never emitted; a symlink takes its target's type, and one that resolves to nothing (dangling, looping) is emitted like a file when it matches.

### 8.2 Path form

Each output path is the **`cwd`-prefixed absolute form** — there is no relative-output toggle. `cwd` is itself resolved to absolute at construction (§5), so output paths are always absolute regardless of the form `cwd` was passed in.

Path separators are POSIX (`/`): the walker constructs paths from dirent names by string concatenation (§7.4), so the only separators present beyond what `path.resolve(cwd)` produced are the `/`s the walker inserted. On Windows, `path.resolve` returns the platform's preferred form (`C:\proj`), followed by the walker's `/`s; one file has one spelling whatever the pattern's shape. Two Windows forms differ, so that every path is one Win32 can open: a `cwd` that already ends in a separator (a drive root, `C:\`) gets none added, and under a verbatim `cwd` (`\\?\C:\proj`) the walker joins with `\`, since Win32 reads no `/` in such a path.

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
  // Both: the `GlobError` or the Node error underneath; none for an
  // ignore entry that starts with `!`.
  readonly cause?: Error;
}
```

### 9.2 `InvalidPattern`

Raised during `prepare()` when any positive or `ignore` pattern fails to compile, or an `ignore` entry starts with `!`: `globSync` throws, `glob` rejects its Promise. The walker catches the matcher's `GlobError` (GLOB_SPEC.md §6) and re-throws it wrapped in `WalkError("InvalidPattern")` so callers have a single error type to match on at the walker layer.

### 9.3 `Io`

Thrown when:

- `fs.statSync(cwd)` fails during §5 cwd validation;
- the resolved `cwd` is not a directory;
- a `readdirSync` (sync path) or `fs.readdir` callback (async path) reports an error. Two errors are not the folder's. A folder its parent listed may be gone by the time it is read (ENOENT: removed; ENOTDIR: replaced by a file): it is left out, as if it had gone before its parent was read, as fast-glob, tinyglobby and node-glob leave it out (walkdir and `ignore` yield an error and go on). `cwd` is checked first (§5), so a missing `cwd` still fails. And where a filesystem keeps no entry types (ext4 without `filetype`, XFS `ftype=0`, some FUSE and network mounts), `readdir` lstats each entry itself and fails with ENOENT from `lstat` when one vanishes in between. The walker then reads the names alone and lstats each, leaving out the ones that are gone, as if they had gone before the read. An entry that cannot be typed for another reason is the folder's error: in both runtimes it names the folder being read.

The async path REJECTS the returned Promise with the first such error and ignores subsequent late callbacks (§7.3). The sync path THROWS, abandoning any in-progress traversal.

The walker MUST NOT silently swallow IO errors (no `onError` callback, no log-and-continue). Rationale: silent swallow would mask the most common failure mode — a typo'd `cwd` — by producing an empty result, which is the worst possible UX. Callers that explicitly want to ignore unreadable subtrees MUST `try`/`catch` and decide what to do at their layer.

This is the walker's most visible divergence from `tinyglobby` and `fast-glob`, tinyglobby suppresses IO errors (`fdir`'s `suppressErrors`) and fast-glob suppresses `ENOENT`.

### 9.4 Stat failures during seeding

A seed's parts are `lstat`-ed one by one (§6 step 2), and the failures are classified, not blanket-swallowed — the rule is root-walk equivalence:

- **Nothing can be there** — silently skipped. That is ENOENT (missing), ENOTDIR (a part is a file), ELOOP (a part is a looping link), ENAMETOOLONG (no directory lists such a name), EINVAL or a NUL in the name (no such name exists). A root walk lists none of these, so this is "this branch of the brace did not match anything that exists on disk", a legitimate "nothing to walk on this branch" outcome rather than something the user typo'd.
- **Anything else (EACCES, …)** — surfaced as `WalkError("Io")`, naming the part that failed. A root walk reaching this subtree fails loudly (§9.3); a seed must not turn that into a silent empty result.

A part that `lstat` finds is judged by §7.1 like any entry, so a broken or looping symlink is a non-directory emitted when it matches (§10), as in the walk.

Permissions are the one place a seed cannot see what a root walk would. A seed `lstat`s its parts, while a root walk lists every folder on the way. So under a folder that can be entered but not listed (mode `0100`) a seed finds its file where a root walk fails, and under one that can be listed but not entered (mode `0400`) a seed fails where a root walk lists the names. Both are rare, and both surface rather than hide a problem.

---

## 10. Symlink Behavior

Symbolic-link entries are surfaced by `readdir`'s `withFileTypes` result with `isSymbolicLink() === true` and `isDirectory() === false` — the dirent does not pre-resolve the link target. The walker's behavior depends on `followSymlinks`:

- `followSymlinks: false`: the symlink is dropped entirely — NOT emitted as a file, NOT descended. Matches `fdir`'s `excludeSymlinks` mode. Each entry is judged when its parent is read: a folder replaced by a symlink after that is read through the link (Node and Rust's `std` read a folder by path, with no `O_NOFOLLOW`), so this option is no guard against a tree that changes during the walk.
- `followSymlinks: true` (default): the walker calls `fs.statSync` on the symlink (NOT `lstatSync`), which follows it. If the result is a directory, the walker treats the symlink as a directory entry and descends into the target's contents. If the stat fails (broken link, permission error), the entry stays a link, a non-directory, and the walk continues. Stat failures here do NOT throw `WalkError("Io")` — broken symlinks are too common in real trees to make them fatal, even in the loud-error default.

**Cycle detection.** When `followSymlinks: true`, the walker MUST break symlink cycles: a frame carries the links descended through to reach it (§7.1), each with the identity of its target, and a link to a folder already among them is not descended. The descent is suppressed for that entry only; the link names a directory, so it is not emitted (§8.1), and the rest of the walk continues.

A folder's identity is its `(dev, ino)`, which the `statSync` of step 4 gave for free (Rust: `MetadataExt::dev`/`ino`); a hit is confirmed by comparing the two links' `fs.realpathSync.native` paths (Rust: `fs::canonicalize`), since some file systems hand out inode numbers that are not unique, and a link whose path cannot be resolved then is not descended. So a walk without a cycle costs no `realpath` at all — a pnpm `node_modules` holds one symlink per package. On Windows a stat carries no such identity, and the walker compares `realpathSync.native` paths alone: the on-disk spelling (the non-native `realpathSync` keeps a link's own spelling, so on a case-insensitive disk `../A` and `../a` would look like two folders); a link whose path cannot be resolved is not descended.

Output paths preserve the symlink path as discovered through `readdir`. `fs.realpath` is called purely for cycle detection — the resolved path is NEVER substituted into the output.

**Cost.** The cycle break bounds each way down, not the walk. A folder that links reach by several routes is read once per route, and each route gives paths of its own, so links that lead the same way more than once multiply the work: fifteen folders, each but the last holding two links to the next, give `**/f` 32,767 paths, and one more folder doubles them. Every walker that follows links answers the same (fast-glob, tinyglobby, node-glob's `follow`, walkdir, `find -L`); one that reads each real folder once (fdir's `withSymlinks()`) is bounded, but its output is real paths, which the paragraph above rules out. A tree that others control (a cloned repository, an unpacked archive, a folder another user can write) is walked with `followSymlinks: false`.

---

## 11. Differences from Other Walkers

| Behavior                          | Us                                  | fast-glob                     | tinyglobby                   | globby                       | doublestar   |
| --------------------------------- | ----------------------------------- | ----------------------------- | ---------------------------- | ---------------------------- | ------------ |
| Concurrent async readdir          | ✅ (own impl)                       | ✅ (`@nodelib/fs.walk` async) | ✅ (`fdir`)                  | ✅ (uses fast-glob)          | n/a          |
| `onlyFiles` default               | hardwired `true`                    | configurable, default `true`  | configurable, default `true` | configurable, default `true` | configurable |
| IO errors throw                   | ✅ (default, no opt-out)            | ✅ (an unreadable folder throws `EACCES`) | ❌ (silent, via `fdir`)      | ✅ (as fast-glob)            | configurable |
| Bad-cwd throws                    | ✅ (validated upfront)              | a file `cwd` throws           | ❌ (returns empty)           | a file `cwd` throws          | varies       |
| `!`-prefix splits to ignore       | ✅ (by `!` parity: `!!x` is `x`)    | ✅ (one `!`)                  | ✅ (one `!`)                 | ✅ (`!!x` ignores `x`; an all-`!` list expands to `**/*` first) | ❌           |
| Static-prefix seeding             | ✅ (the literal heads of all patterns, one seed per covering head) | ✅                            | ✅                           | ✅                           | ✅           |
| `withFileTypes` option            | ❌ (drop entirely)                  | ✅                            | ❌                           | ✅                           | ✅           |
| `onError` callback                | ❌ (errors throw)                   | ❌                            | ❌                           | ❌                           | ❌           |
| `followSymlinks` default          | `true`                              | `true`                        | `true`                       | `true`                       | `true`       |
| Symlink cycle detection           | ✅ (target identity + ancestor chain) | ❌                          | ✅ (`fdir`'s `isRecursive`)  | ❌                           | ❌           |
| `followSymlinks: false` semantics | drop entirely (no emit, no descend) | emit-as-file                  | drop entirely                | emit-as-file                 | varies       |
| Dangling symlink                  | emitted as a file when it matches   | dropped                       | dropped                      | dropped                      | not measured |

The walker is opinionated about errors: any failure throws by default. Combined with hard-wired files-only output, hard-wired absolute output, and a 5-option surface, this trades configurability for predictability — fewer modes means fewer surprises.

---

## 12. Appendix A: Glossary

- **Walker.** The filesystem-traversal layer (`glob` / `globSync`) that invokes the matcher to enumerate matching file paths under `cwd`.
- **Frame.** One unit of walker work: an absolute directory path paired with its relative-to-`cwd` form and the links descended through to reach it. The traversal stack / scheduler holds frames.
- **Seed frame.** A frame produced from the matcher's static prefixes during initialization, before any `readdir` has been issued.
- **Auto-split.** The walker's preprocessing of a `patterns` array that routes `!`-prefixed entries to the ignore set (§4.2).
- **Static prefix.** The literal head of one expansion of the pattern's braces, up to its last whole segment (GLOB_SPEC.md §17). Walker uses these to skip directories no expansion can match.
- **`withSep`.** A folder's absolute path with a separator after it; a child's path is that and its name, bypassing `path.join`'s normalization for measurable speedup (§7.4).
- **Late callback.** An `fs.readdir` callback that fires after the walker has already rejected its returned Promise. The async traversal MUST drop these (§7.3).
