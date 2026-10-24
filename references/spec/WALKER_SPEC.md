# WALKER_SPEC: The Filesystem Walker

**Status.** This document is the normative definition of the filesystem walker built on the glob matcher of [GLOB_SPEC.md](./GLOB_SPEC.md). The pattern language, matching and directory matching are defined there; this document defines only what the walker adds: its inputs, the preprocessing of patterns, the traversal, the output and the errors.

**Reference implementation.** The JS package `@globstar/walk`. The Rust crate `globstar-walk` has a different interface (an iterator of entries and errors) and follows this document where its own documentation says so.

**Conventions.** The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119. Paragraphs headed *Remark* are informative.

## Contents

1. [Scope](#1-scope)
2. [Operations](#2-operations)
3. [Options](#3-options)
4. [Pattern Preprocessing](#4-pattern-preprocessing)
5. [The Base Directory](#5-the-base-directory)
6. [Seeding](#6-seeding)
7. [Traversal](#7-traversal)
8. [Output](#8-output)
9. [Errors](#9-errors)
10. [Symbolic Links](#10-symbolic-links)
11. [Differences from Other Walkers](#11-differences-from-other-walkers)
12. [Glossary](#12-glossary)

---

## 1. Scope

### 1.1 In scope

- Two enumeration operations, asynchronous and synchronous, that return the paths of the files under a base directory that a set of patterns selects.
- Pattern preprocessing: the routing of `!`-prefixed patterns into an ignore set and the normalization of each pattern to the spelling of walked paths.
- Traversal driven by the matcher's `match_dir` and `static_prefixes` (GLOB_SPEC §15, §17).
- The output form: files only, as absolute paths under the base directory.
- The error model: pattern and I/O failures are reported, with the exceptions of §9.3, §9.4 and §10.

### 1.2 Out of scope

- The pattern language and its semantics (GLOB_SPEC.md).
- Gitignore-style re-inclusion, where a later rule cancels an earlier one.
- Filters on file contents.
- Watching for changes.

---

## 2. Operations

The walker offers two operations: an asynchronous enumeration, which returns a promise of the result, and a synchronous one, which returns the result directly. Both take the same inputs (a pattern or an iterable of patterns, and the options of §3), produce the same set of paths in the same form (§8), and fail in the same ways (§9). They differ only in how directory reads are sequenced (§7.2, §7.3) and therefore in the order of the result (§7.5).

---

## 3. Options

### 3.1 Surface

| Option            | Type                          | Default | Meaning                                              |
| ----------------- | ----------------------------- | ------- | ---------------------------------------------------- |
| `cwd`             | string                        | `"."`   | the base directory (§5)                              |
| `dot`             | boolean                       | `false` | GLOB_SPEC §3.1, forwarded to every matcher           |
| `caseInsensitive` | boolean                       | `false` | GLOB_SPEC §3.2, forwarded to every matcher           |
| `followSymlinks`  | boolean                       | `true`  | §10                                                  |
| `ignore`          | string or array of strings    | `[]`    | §3.2; a bare string is a one-element array           |

An option present with the value `undefined` means its default. Only the object's own keys are read. The walker MUST reject with a `TypeError` an options value that is not an object (`null` and `undefined` mean all defaults), a key outside the five above, a non-boolean `dot`, `caseInsensitive` or `followSymlinks`, a non-string `cwd`, and an `ignore` that is neither a string nor an array of strings. The synchronous operation throws the error; the asynchronous one rejects its promise.

### 3.2 Semantics

**`cwd`.** The directory against which the patterns and the `ignore` entries are resolved, and the root of the traversal. It is resolved to an absolute path when the operation starts (§5); a later change of the process's working directory does not affect the walk.

**`dot`.** Forwarded to the matcher. The walker's default is `false`, the convention under which `**/*` does not enter `.git` or `.cache`. This is the one option whose default differs between the matcher (`true`) and the walker (`false`): hiding dotfiles is a filesystem convention, not a property of the pattern language.

**`caseInsensitive`.** Forwarded to the matcher (ASCII only, GLOB_SPEC §13). §5 states how seeding behaves under it.

**`followSymlinks`.** Whether a symbolic link is read through to its target (§10). With `false`, a link is neither returned nor entered.

**`ignore`.** A second set of patterns. A path is selected if and only if

```
selected(path) := positive(path)
                  ∧ ¬ignore(path)
                  ∧ ∀ d strictly between cwd and path.  ¬ignore(d) ∧ ¬ignore(d · "/")
```

where `positive` and `ignore` are the matchers of §4.4. An ignored directory hides its whole subtree: with `ignore: ["dist"]`, the pattern `**/*.js` does not return `dist/a.js`, although `dist` names no `.js` file.

The `ignore` entries are compiled with the same `dot` and `caseInsensitive` as the positive set. A directory is asked about with a trailing `/` as well as without one (§7.1), so an entry that ends in `/` (also after the normalization of §4.3, since `d/.` ≡ `d/`) matches directories only: `node_modules/` prunes the directory `node_modules` and everything below it, never a file of that name. Like every pattern it is anchored at `cwd`; `**/node_modules/` prunes at any depth. An entry that ends in `/**` prunes the directory too, since `node_modules/` is a path below `node_modules`; a dot-led name below such a directory is then not found under either `dot`, as fast-glob and tinyglobby do not find it. A positive pattern that ends in `/` matches directories only and therefore selects no file (§8.1).

Each `ignore` entry is a positive pattern. The set is not subject to the split of §4.2, and an entry that starts with `!` after the normalization of §4.3 is the error `InvalidPattern` (§9.2): as a whole-pattern negation (GLOB_SPEC §11.1) it would ignore everything except what it names, which is never what an ignore list means. A name that starts with `!` is written `\!`.

### 3.3 No other options

The walker has these five options and no others. In particular, there is no switch for directories in the output (the output is files only, §8.1), for relative output (the output is absolute, §8.2), for an error callback (errors are reported, §9) or for entries with file types (the output is strings, §8.3).

---

## 4. Pattern Preprocessing

### 4.1 Input

The patterns are a string or an iterable of strings (an array, a set), read once. A string is treated as a one-element list. Each pattern and each `ignore` entry is first checked for length as written (§9.2).

### 4.2 The `!` split

Each pattern of the input, after the normalization of §4.3 (so that a leading `/` does not shield a `!`: `/!x` ≡ `!x`), is routed by the parity of its leading run of `!`, the rule of GLOB_SPEC §11.1:

- an odd count: every `!` is removed and the body joins the ignore set;
- an even count: every `!` is removed and the body joins the positive set.

The body is normalized again after the removal (`!/x` exposes a new leading `/`), and a `!` run that this exposes is counted as well, so the count covers every `!` that leading `/`s, `.` segments and `!`s hide: `!/!x` ≡ `!!x` is the positive pattern `x`.

| Input           | `!` count | Body           | Set      |
| --------------- | --------- | -------------- | -------- |
| `src/**/*.ts`   | 0         | `src/**/*.ts`  | positive |
| `!**/*.test.ts` | 1         | `**/*.test.ts` | ignore   |
| `!!**/*.ts`     | 2         | `**/*.ts`      | positive |
| `!!!**/*.ts`    | 3         | `**/*.ts`      | ignore   |
| `!/!x`          | 2         | `x`            | positive |

So `["src/**/*.ts", "!**/*.test.ts"]` is equivalent to `["src/**/*.ts"]` with `ignore: ["**/*.test.ts"]`. A file name that starts with `!` is written escaped (`\!foo` or `[\!]foo`, GLOB_SPEC §11.3).

### 4.3 Normalization

Each pattern, positive or ignore, from the input or from the `ignore` option, is normalized before compilation, so that patterns are spelled as walked relative paths are spelled:

- a leading run of `/` is removed: `/a/b/*.ts` ≡ `a/b/*.ts`, `//foo` ≡ `foo`;
- a run of `/` collapses to one: `a//b/*` ≡ `a/b/*`;
- a plain `.` segment is removed (a listing never yields `.`): `./src/**` ≡ `src/**`, `a/./b` ≡ `a/b`. A last `.` names the directory before it and becomes a trailing `/`: `d/.` ≡ `d/`, and `*/.` matches no file;
- a pattern that normalizes to nothing (`.`, `./`) matches nothing and is dropped.

An escaped form (`\.`) is untouched. A `/` in a pattern is always structural (`\/` is an error, GLOB_SPEC §4.2), so the textual rewrite is exact.

Patterns are scoped to `cwd`. The removal of the leading `/` keeps every static prefix (§6) relative to `cwd`; an absolute filesystem path cannot be walked through the pattern, and a caller who wants to walk `/etc` passes `cwd: "/etc"`.

### 4.4 Compilation

After preprocessing the walker compiles two matchers (the Rust crate, which yields directories as well, a third for the positive patterns that end in `/`):

- `positive`: the union of the positive set;
- `ignore`: the union of the `ignore` option and the `!`-routed set, or none when both are empty.

When the positive set is empty after preprocessing (an empty list, or one whose entries all route to the ignore set or reduce to nothing), the walker MUST return an empty result without reading the filesystem. `cwd` is still validated and the `ignore` set still compiled, so their errors are reported (§9).

When a matcher does not compile, the walker MUST raise `InvalidPattern` (§9.2) naming the member as the caller wrote it (`!src/[z-a]`, not the body `src/[z-a]`) and the reason. When preprocessing changed the member, the reason names the text its byte offsets refer to.

---

## 5. The Base Directory

The walker MUST resolve `cwd` to an absolute path when the operation starts, by text alone: the empty string is the process's working directory, `.` and `..` segments are removed without consulting the filesystem (`jump/..` is the directory that holds `jump`, even when `jump` is a link), and no link is resolved. The Rust crate resolves its base the same way, so one base string names one directory in both.

The resolved `cwd` MUST be validated: it MUST exist and MUST be a directory. Otherwise the walker raises `Io` with `cwd` as the path (§9.3). A mistyped `cwd` therefore fails at the start rather than producing an empty result.

**Case and seeding.** Seeding (§6) reaches a static prefix by its spelling in the pattern, under `caseInsensitive: true` as well. On a case-sensitive filesystem, a prefix spelled in a case other than the directory's does not resolve and its subtree is not walked: `src/*.rs` with `caseInsensitive: true` returns nothing when the directory is `SRC`, although the matcher accepts `SRC/a.rs`. On a case-insensitive filesystem the prefix resolves through the filesystem's own folding, and the returned paths carry the pattern's spelling. A caller who needs the matcher's behavior on a case-sensitive filesystem writes the prefix in the directory's case, or begins it with a class (`[Ss]rc/*`), which leaves no literal prefix and walks from `cwd`.

The same folding acts the other way under `caseInsensitive: false` on a case-insensitive filesystem: a prefix spelled `src` resolves to a directory `SRC`, so a literal-prefixed pattern returns paths, in the pattern's spelling, that the matcher would reject against the directory's spelling, and prefixes that differ only in case (`{src,SRC,Src}/*`) each return the same file once. This follows from seeding by spelling on a folding filesystem (fast-glob behaves the same), and deduplication by inode would not correct it, since different spellings legitimately select different branches of a pattern. A caller who needs exact-case walking on such a filesystem begins the pattern with a class. On such a filesystem an `ignore` entry does not hold against a pattern that spells its literal head in another case (`.GIT/config` reaches `.git/config` past `**/.git/**`); a caller whose ignore list must hold against patterns from others walks with `caseInsensitive: true`, which folds the ignore set as the filesystem folds names.

---

## 6. Seeding

The matcher's static prefixes (GLOB_SPEC §17) are the literal heads every match starts with, one per brace expansion, at a segment boundary. The walker begins its traversal at them instead of at `cwd`, so that directories no expansion can match are never read.

Seeding MUST be observationally equivalent to a walk from `cwd`: it may skip work and never change the result. A prefix is therefore reached the way a walk from `cwd` reaches it, segment by segment, and each segment is judged by the per-entry rule of §7.1 as if it were the only entry of its directory, with the result of `lstat` standing in for the directory entry. Every rule of the walk then holds for seeds by construction: ignore entries and directory-only entries, the pruning of covered directories, special files, `followSymlinks`, the cycle rule and broken links. For each prefix:

1. The prefix is dropped when any of its segments is `..`, `.` or empty, split at every separator of the platform (on Windows a `\` as well). A listing yields none of these, so no walk from `cwd` reaches such a path, and `..` would leave `cwd`. A brace branch that starts with `/` gives such a prefix: `{a,/b}/*` has the prefixes `a` and `/b`, and `/b` is dropped.
2. Each segment, from `cwd` down:
   - A segment whose spelling could open another entry MUST appear in its directory's listing as written, or the prefix is dropped: a walk from `cwd` compares names as listed and never matches such a spelling. These are a non-ASCII name on a filesystem that ignores Unicode normalization (an NFC spelling opens a name stored as NFD), and on Windows a name Win32 rewrites: one that ends in `.` or a space, a device name (`NUL`, `com1`, `con.txt`), or one that holds a `:`.
   - The segment is `lstat`ed. If nothing can be there (§9.4), the prefix is dropped silently: `{a,b}/foo` returns the results of the branch `a` when only `a/` exists. Any other failure is raised, as the walk would raise it (§9.3).
   - The entry is judged by §7.1. A segment before the last must yield a directory frame, from which the next segment continues; otherwise the prefix is dropped, as a walk from `cwd` would not enter there. The last segment's result is the seed's: a matching file goes to the output, a directory worth walking becomes a seed frame.

Seed frames, and the files seeds emit, keep the prefixes' order (deduplicated and sorted); seed files are emitted before any directory is read. Order beyond that is unspecified (§7.5).

The empty prefix, produced by a pattern that begins with `**/` or has no literal head, seeds one frame at `cwd` itself.

---

## 7. Traversal

### 7.1 The per-entry rule

A frame is a directory to read: its absolute path, its path relative to `cwd`, and the list `links` of the symbolic links entered to reach it, each with the identity of its target (§10). A seed frame carries the links met on its prefix; the frame for `cwd` carries none. A child's relative path is the frame's relative path, a `/` when that path is not empty, and the child's name; its absolute path is formed as §7.4 says. For each entry of the directory, with its name and its type as the listing reports it:

1. **Link drop.** If the entry is a symbolic link and `followSymlinks` is `false`, the entry is skipped: not returned, not entered. This is decided before any other work.
2. **Unnameable entries.** An entry whose name, as decoded, holds U+FFFD and does not exist under that spelling is skipped: its name was not valid UTF-8, no string names it, and no directory can be read through it. The Rust crate, which sees bytes, skips a name that is not valid UTF-8 the same way.
3. **Ignore.** If there is an `ignore` matcher and it matches the child's relative path, the entry is skipped. This precedes any `stat`, so an ignored link is never resolved.
4. **Type.** A regular entry has its own type. A symbolic link (which by step 1 is followed) takes its target's type, found by `stat`; when `stat` fails (a dangling or looping link) the entry remains a link, a non-directory (§10).
5. **Special files.** A FIFO, a socket or a device, which is neither a directory, a file nor a link, is skipped (§8.1).
6. **Branch on the type.**
   - A non-directory is returned when `positive` matches its relative path.
   - A directory is read only for the files below it. It is skipped when `ignore` matches its relative path with a trailing `/` (§3.2), when `may_match_below` of `positive.match_dir` on its relative path is false, or when `ignore.match_dir` on its relative path reports `matches_all_below`. The last test applies under `dot: true` only: under `dot: false` a `**` never covers the dot-led names below (GLOB_SPEC §15.5), which a positive pattern may still name. Otherwise a child frame is created, whose `links` are the frame's, plus this entry when it is a link to a directory. A link whose target is already the target of a link in the frame's `links` closes a cycle and is not entered (§10).

### 7.2 The synchronous operation

The synchronous operation reads one directory at a time, depth first: the seed frames in order, and under each directory its child frames in the order the listing gave them. A failure that §9.3 raises ends the operation with that error; results gathered so far are discarded.

### 7.3 The asynchronous operation

The asynchronous operation may read several directories at once; every seed frame and every child frame may be read as soon as it is known. The operation resolves with the result when every read has completed. The first failure that §9.3 raises rejects the promise with that error; a read that completes after the rejection MUST NOT reject again, MUST NOT start further reads and MUST NOT change the result.

### 7.4 Path joining

A child's absolute path is its directory's absolute path, one separator (§8.2; none when the directory's path already ends in a separator) and the child's name. No other joining or normalization takes place: only names from listings and prefix segments guarded by §6 step 1 are appended to the resolved `cwd`.

### 7.5 Order

- The synchronous operation returns the files in depth-first order, the entries of each directory in the order of its listing.
- The asynchronous operation returns the entries of one directory in the order of its listing; the order across directories is unspecified.
- A caller who needs a fixed order sorts the result.

---

## 8. Output

### 8.1 Files only

The result holds file paths only. A directory is never returned, even when a pattern matches it in the matcher's sense; this is not configurable. A FIFO, a socket or a device is never returned. A symbolic link takes its target's type, and a link that resolves to nothing (dangling or looping) is returned like a file when it matches.

### 8.2 Path form

Each path is absolute, formed from the resolved `cwd` (§5) and the names below it. The separators the walker inserts are `/`; the resolved `cwd` keeps the platform's form (`C:\proj` on Windows). A `cwd` that ends in a separator (`/`, a drive root `C:\`) gets none added. Under a verbatim `cwd` on Windows (`\\?\C:\proj`), where Win32 reads no `/`, the walker inserts `\`. One file has one spelling whatever the pattern's shape. A caller who needs paths relative to `cwd` derives them from the result.

### 8.3 Result

The result is a fresh array of strings, allocated per call.

---

## 9. Errors

### 9.1 Kinds

A walker error has a kind, `InvalidPattern` or `Io`, and the following fields:

| Kind             | Fields                                                              |
| ---------------- | ------------------------------------------------------------------- |
| `InvalidPattern` | `pattern`, the member as written; `reason`; `cause`, the matcher's error, when there is one |
| `Io`             | `path`, the path that failed; `cause`, the underlying error         |

The synchronous operation throws the error; the asynchronous one rejects its promise.

### 9.2 `InvalidPattern`

Raised before any read, when a positive or `ignore` pattern does not compile, is too long as written, or is an `ignore` entry that starts with `!` (§3.2). The matcher's error (GLOB_SPEC §6) is the cause.

A pattern or `ignore` entry longer than 65,536 bytes of UTF-8 as written is refused before the split of §4.2 and the normalization of §4.3, which could hide its length (`/` repeated 100,000 times and then `x` normalizes to `x`). The `cause` is the matcher's `TooLong` for that length, and the `pattern` field holds the first 64 characters of the text followed by `…`. Every pattern an error quotes is therefore within the limit.

### 9.3 `Io`

Raised when `cwd` does not exist or is not a directory (§5), and when a directory read fails, with two exceptions that are not the directory's failure:

- a directory its parent listed may be gone when it is read (removed, or replaced by a file): it is left out, as if it had gone before its parent was read, as tinyglobby and node-glob leave it out;
- on a filesystem that keeps no entry types, a listing types each entry by `lstat` and fails when one vanishes in between: the walker then reads the names alone, types each by `lstat`, and leaves out the ones that are gone.

An entry that cannot be typed for another reason is the directory's failure, and the error names the directory being read.

The walker MUST NOT silence an I/O error: there is no error callback and no log-and-continue. A caller who accepts unreadable subtrees catches the error and decides at its own level.

*Remark.* A silenced error would turn the most common mistake, a mistyped `cwd`, into an empty result.

### 9.4 `stat` failures during seeding

A prefix's segments are `lstat`ed one by one (§6), and a failure is classified by whether anything can be there:

- Nothing can be there: the prefix is dropped silently. These are ENOENT (missing), ENOTDIR (a segment is a file), ELOOP (a looping link), ENAMETOOLONG (no directory lists such a name), EINVAL and a NUL byte in the name. A walk from `cwd` lists none of these; the branch of the pattern matches nothing that exists.
- Anything else (EACCES, …): `Io`, naming the segment that failed. A walk from `cwd` reaching that subtree would fail, and a seed must not turn the failure into an empty result.

A segment that `lstat` finds is judged by §7.1 like any entry, so a broken or looping link is a non-directory, returned when it matches (§10).

Permissions are the one case where a seed and a walk from `cwd` see different things: a seed `lstat`s its segments, while a walk lists every directory on the way. Under a directory that can be entered but not listed, a seed finds its file where the walk fails; under one that can be listed but not entered, a seed fails where the walk lists the names. Both cases report a problem rather than hide one.

---

## 10. Symbolic Links

A listing reports a symbolic link as a link, without resolving its target.

- `followSymlinks: false`: a link is skipped entirely, neither returned nor entered. Each entry is judged when its parent is read, and a directory replaced by a link after that is read through the link, so this option is no protection against a tree that changes during the walk.
- `followSymlinks: true` (the default): the link is `stat`ed, which follows it. A link to a directory is treated as a directory and its target's entries are read; a link whose `stat` fails (dangling, looping, inaccessible) remains a link, a non-directory, and the walk continues. These failures do not raise `Io`.

**Cycles.** With `followSymlinks: true` the walker MUST break cycles: a frame carries the links entered to reach it (§7.1), each with its target's identity, and a link to a directory already among them is not entered. Only that descent is suppressed; the link names a directory and is not returned (§8.1), and the walk continues.

A directory's identity is its device and inode numbers, as the `stat` of §7.1 step 4 reports them; a hit is confirmed by comparing the two links' real paths, since some filesystems report inode numbers that are not unique, and a link whose real path cannot be found is not entered. A walk without a cycle resolves no real path. On Windows a `stat` carries no such identity, and the walker compares real paths alone, in the on-disk spelling (so that `../A` and `../a` on a case-insensitive disk are one directory); a link whose real path cannot be found is not entered.

Returned paths keep the link's path as listed; a real path is never substituted into the output.

*Remark.* The cycle rule bounds each route down, not the walk. A directory that links reach by several routes is read once per route, and each route yields paths of its own, so links that lead the same way more than once multiply the work: fifteen directories, each but the last holding two links to the next, give `**/f` 32,767 paths. Every walker that follows links and reports link paths behaves the same (fast-glob, tinyglobby, node-glob with `follow`, walkdir, `find -L`). A tree that others control is walked with `followSymlinks: false`.

---

## 11. Differences from Other Walkers

| Behavior                          | globstar                                        | fast-glob                                   | tinyglobby                   | globby                                                   | doublestar   |
| --------------------------------- | ----------------------------------------------- | ------------------------------------------- | ---------------------------- | -------------------------------------------------------- | ------------ |
| Concurrent directory reads        | ✅                                              | ✅ (`@nodelib/fs.walk`)                     | ✅ (`fdir`)                  | ✅ (fast-glob)                                           | n/a          |
| Files only                        | always                                          | configurable, default files                 | configurable, default files  | configurable, default files                              | configurable |
| I/O errors are reported           | ✅ (no opt-out)                                 | ✅ (an unreadable directory throws `EACCES`) | ❌ (silent, via `fdir`)      | ✅ (as fast-glob)                                        | configurable |
| A bad `cwd` is reported           | ✅ (validated first)                            | a file `cwd` throws                         | ❌ (empty result)            | a file `cwd` throws                                      | varies       |
| `!` prefix routes to ignore       | ✅ (by parity: `!!x` is `x`)                    | ✅ (one `!`)                                | ✅ (one `!`)                 | ✅ (`!!x` ignores `x`; an all-`!` list expands to `**/*`) | ❌           |
| Seeding from static prefixes      | ✅ (every pattern's heads, one seed per covering head) | ✅                                     | ✅                           | ✅                                                       | ✅           |
| Entries with file types           | ❌                                              | ✅                                          | ❌                           | ✅                                                       | ✅           |
| Error callback                    | ❌                                              | ❌                                          | ❌                           | ❌                                                       | ❌           |
| `followSymlinks` default          | `true`                                          | `true`                                      | `true`                       | `true`                                                   | `true`       |
| Link cycles broken                | ✅ (target identity along the route)            | ❌                                          | ✅ (`fdir`)                  | ❌                                                       | ❌           |
| `followSymlinks: false`           | link skipped                                    | returned as a file                          | link skipped                 | returned as a file                                       | varies       |
| Dangling link                     | returned as a file when it matches              | dropped                                     | dropped                      | dropped                                                  | not measured |

---

## 12. Glossary

- **Walker.** The traversal layer that uses the matcher to enumerate the files under `cwd` that a set of patterns selects.
- **Frame.** One unit of work: a directory's absolute path, its path relative to `cwd`, and the links entered to reach it.
- **Seed frame.** A frame made from a static prefix before any directory is read (§6).
- **Split.** The routing of `!`-prefixed input patterns to the ignore set (§4.2).
- **Static prefix.** The literal head of one expansion of a pattern, up to its last whole segment (GLOB_SPEC §17).
