# 04. The Walker

The walker enumerates the files under a base directory that a set of patterns selects, reading as few directories as the matcher allows. WALKER_SPEC.md states its contract; this note describes the two implementations: the JS package `packages/globstar-walk/src/walk.js`, with an asynchronous and a synchronous operation, and the Rust crate `crates/globstar-walk/src/lib.rs`, an iterator. Both apply the same per-entry rule (§4) and differ in scheduling (§5) and in what they yield (the Rust walker yields matching directories as well).

## 1. Structure

A walk holds two matchers and the base:

| Field          | Meaning                                                                                 |
| -------------- | --------------------------------------------------------------------------------------- |
| `matcher`      | the union of the positive patterns                                                      |
| `ignore`       | the union of the `ignore` option and the `!`-routed patterns, or none                   |
| `dirs` (Rust)  | the union of the positive patterns that end in `/`, which match directories only        |
| `cwd` / `base` | the base directory, resolved to an absolute path                                        |
| `follow_links` | whether a link is read through to its target                                            |
| `prune_ignored`| whether the ignore set may cover a subtree (`dot: true`; under `dot: false` it never does) |

The unit of work is a *frame*: a directory's absolute path, its path relative to the base (with `/`), and the links entered to reach it (§7). The JS walk keeps its seed frames and seed results in a context; the Rust walk is a struct with a stack of frames, the directory being read, a buffer of seed results, and a scratch buffer for the relative path of the entry being judged.

## 2. Preprocessing

**Normalization** (`normalize_pattern`, WALKER_SPEC §4.3). The leading `/`s are removed, runs of `/` collapse, `.` segments are dropped, and a last `.` becomes a trailing `/`. A pattern that normalizes to nothing selects nothing and is dropped; the written empty string is kept for the matcher to refuse.

**The `!` split** (`split_bangs`, WALKER_SPEC §4.2). The normalized pattern's leading `!`s are counted and removed; when removing them leaves a `.` segment or an empty one, that segment goes with its `/`, and a `!` run it exposes is counted as well. The parity of the count routes the body to the positive or the ignore set. Each body keeps the text it came from, for error messages.

**Length** (`check_length`, WALKER_SPEC §9.2). A written pattern or ignore entry longer than 65,536 bytes is refused before it is normalized, and named by its first 64 characters. The JS walker measures UTF-8 bytes only for a string that could be that long (more than a third of the limit in UTF-16 units).

**Compilation** (`compile_union`). The positive set, then the ignore set, each as one union; an ignore entry that starts with `!` after normalization is refused. A member that does not compile is reported as the caller wrote it, with the matcher's error (`InMember` names the member of a union) and, when normalization changed the member, a note naming the text the error's offsets refer to. The base is resolved and validated before the empty-positive-set return, so a bad base is reported whatever the patterns.

**The base.** `path.resolve` in JS; `std::path::absolute` followed by a textual removal of `.` and `..` in Rust (`lexical`), so that both runtimes resolve one base string to one directory without consulting the filesystem.

## 3. Seeding

`init_from_prefixes` turns the matcher's static prefixes (note 03, §14) into seed frames. A walk from the base would reach each prefix by judging one entry per segment; seeding judges the same entries without reading the directories on the way.

```
for each prefix p of matcher.static_prefixes():
    if p = "":  seed the base itself; continue
    split p at every separator of the platform
    if a segment is "", "." or "..":  drop p          (no listing yields these)
    frame ← the base
    for each segment, in order:
        if the segment's spelling could open another entry and it is not in the
           listing of frame as written:  drop p         (§3.1)
        lstat(frame / segment)
            nothing can be there (ENOENT, ENOTDIR, ELOOP, ENAMETOOLONG, EINVAL):  drop p
            any other failure:  report Io for that path; drop p
        judge the segment by the per-entry rule (§4) as the only entry of frame
        if it is the last segment:  its file goes to the results, its frame to the seeds
        else if it gave no child frame:  drop p
        else frame ← that child frame
```

The Rust walker seeds on the first `next()`, so the seeds see the tree the walk sees. Seed results are yielded before any directory is read.

### 3.1 Names that open another entry

A name reaches an entry its directory lists under another spelling on a filesystem that ignores Unicode normalization (APFS opens an NFC spelling of a name stored NFD) and under Win32 for a name it rewrites: one that ends in `.` or a space, a device name (`CON`, `PRN`, `AUX`, `NUL`, `COMn`, `LPTn`, with any extension), or one that holds a `:`. Such a segment is accepted only when the directory lists it as written (`opens_as_another`, `listed`, `win32_rewrites`); a directory that cannot be read leaves the decision to the `lstat`.

**Proposition 1 (seeding is invisible).** The files a seeded walk returns are the files a walk from the base returns, under the permission caveat of WALKER_SPEC §9.4.

*Proof sketch.* By note 03, Proposition 2, every file the pattern matches lies below some static prefix, so a walk from the base returns no file outside the seeded subtrees. Within a subtree, both walks read the same directories and apply the same rule to the same entries. On the way to a prefix, the seeded walk judges each segment by the rule a walk from the base would apply to the same entry when its parent is read, with the `lstat` of the segment standing in for the listing's type; the guard of §3.1 ensures the segment is an entry that listing holds, and a segment that nothing can be at drops the prefix, as a walk from the base would never list it. The two walks therefore reach the same frames and return the same files. ∎

## 4. The per-entry rule

`Rules::judge` (Rust) and `processDirents` (JS) apply WALKER_SPEC §7.1 to one entry of a frame. The order is the one that costs least: tests that need no system call come first.

```
if the entry is a link and not follow_links:           skip
if the name is not valid UTF-8:                        skip
rel ← frame.relative, "/", name
if ignore matches rel:                                 skip      (before any stat)
if the entry is a link:  type ← stat(frame / name), or the link itself when stat fails
if the type is a FIFO, socket or device:               skip
if the type is not a directory:
    if matcher matches rel:  yield the file
    done
if ignore matches rel · "/":                           skip      (entries ending in "/" or "/**")
dm ← matcher.match_dir(rel)
(Rust) if dm.matches or dirs matches rel · "/":        yield the directory
if not dm.may_match_below:                             done
if prune_ignored and ignore.match_dir(rel).matches_all_below:  done
if the entry is a link:
    if its target is already among frame.links:        done      (a cycle, §7)
    links ← frame.links + this link
child frame ← (frame / name, rel, links)
```

The ignore test precedes the `stat` so that an ignored link is never resolved. The absolute path of an entry is built only when it is yielded or entered. The test with the trailing `/` catches ignore entries that end in `/` or `/**` under either `dot`; the test with `match_dir` catches entries such as `**/node_modules/**/*`, which the first cannot, and is made under `dot: true` only, since under `dot: false` a `**` never covers the dot-led names below (GLOB_SPEC §15.5).

The soundness of the two pruning tests is note 03, Propositions 3 and 4: a directory skipped on `may_match_below = false` holds no match, and one skipped on the ignore set's `matches_all_below` holds no path the ignore set would let through.

## 5. Scheduling

**Synchronous, JS.** A stack of frames, the seed frames reversed so the first pops first. A frame is read whole, its entries pass the rule, and its child frames are pushed in reverse so that the first child pops next: depth first, the entries of each directory in the listing's order.

**Rust.** The same stack, but a directory is read entry by entry: `Reading` holds the open listing, and `next()` yields each entry's result as it is judged, so a caller that stops early reads no more. Child frames are pushed as they are found, in the listing's order, and the slice pushed during the directory is reversed when the directory is done (`mark` remembers where it starts), so the first child is read next. Memory follows the directories found, not the entries read. The iterator is fused: once the stack and the open listing are empty, every later call returns nothing.

**Asynchronous, JS.** Every seed frame is submitted at once, and every child frame as soon as the rule finds it; `submit` issues the read and counts it in `pending`. A read's callback decrements `pending`, applies the rule to the entries, submits the children, and resolves the promise when `pending` reaches zero. The first failure sets `failed` and rejects; a callback that arrives after that returns at once, so it neither rejects again nor submits more reads nor touches the result. Reads overlap on the runtime's thread pool; the order across directories is whatever order the reads complete in.

## 6. Errors

A directory read that fails is classified (`after_read_error`, WALKER_SPEC §9.3):

- ENOENT or ENOTDIR: the directory its parent listed is gone, or is now a file; it is left out, as if it had gone before its parent was read. The base was validated first, so this never hides a missing base.
- ENOENT from the `lstat` a typeless filesystem's listing makes: an entry vanished between the listing and its typing. The directory is read again by names alone (`read_each_entry`), each name typed by its own `lstat`, and a name that is gone is left out. The Rust walker meets the same case as a `NotFound` from `DirEntry::file_type` and skips that entry.
- Anything else is `Io`, naming the directory being read. The synchronous walks stop; the asynchronous one rejects.

A `stat` that fails while typing a link does not raise: the entry stays a link (§7). A `lstat` that fails during seeding is classified by §3.

## 7. Symbolic links

With `follow_links`, a link is typed by `stat`, which follows it, and a link to a directory is entered. Each frame carries the links entered to reach it, each with its target's identity: on Unix the target's `(dev, ino)`, which the typing `stat` gave, together with the link's own path; on Windows the target's canonical path. A link whose target is already among the frame's links closes a cycle and is not entered; the rest of the walk continues. An identity hit on Unix is confirmed by comparing the two links' canonical paths, since some filesystems hand out inode numbers that are not unique; a walk without a cycle therefore resolves no canonical path. Yielded paths keep the link's path; the canonical path is used for the cycle test only.

The Rust walker keeps a frame's links as a shared list (`Links`, an `Arc` cons list): a child frame reached without a link shares its parent's list, and one reached through a link prepends to it, so each link is held once however many directories wait below it. The JS walker copies the array when it appends.

The cycle test bounds each route down, not the walk: a directory that links reach by several routes is read once per route, and the paths of each route are yielded (WALKER_SPEC §10).

## 8. Paths

A child's absolute path is its parent's with one separator and the name (`join`, `with_sep`): no normalization, since only listed names and the guarded segments of §3 are appended to the resolved base. The JS walker inserts `/`, or `\` under a verbatim Windows path (`\\?\C:\…`), and nothing after a parent that already ends in a separator; the Rust walker lets `PathBuf::push` insert the platform's separator. Relative paths are built with `/` in both, so the matcher sees one spelling on every platform.

## 9. Complexity

The walk reads each directory it enters once per route to it, and makes one `stat` per link met with `follow_links`, one `lstat` per seed segment, and one listing per seed segment whose spelling could open another entry. The matcher is asked at most three times per directory (`ignore` without and with the `/`, and `match_dir` twice at most) and once or twice per file. Memory is the frames found and not yet read: the depth of the tree times the fan-out in the synchronous walks, and the number of reads in flight in the asynchronous one.

## 10. Source map

| Symbol                                                                                           | File                                   |
| ------------------------------------------------------------------------------------------------ | -------------------------------------- |
| `Walk`, `Frame`, `Reading`, `Rules::judge`, `Rules::visit`, `init_from_prefixes`, `read_next`     | `crates/globstar-walk/src/lib.rs`      |
| `normalize_pattern`, `split_bangs`, `compile_union`, `check_length`, `lexical`                   | `crates/globstar-walk/src/lib.rs`      |
| `Links`, `Link`, `link_to_descend`, `opens_as_another`, `listed`, `win32_rewrites`, `nothing_there`, `gone` | `crates/globstar-walk/src/lib.rs` |
| `WalkOptions`, `DirEntry`, `WalkError`                                                            | `options.rs`, `entry.rs`, `error.rs`   |
| `glob`, `globSync`, `prepare`, `initFromPrefixes`, `processDirents`                               | `packages/globstar-walk/src/walk.js`   |
| `normalizePattern`, `splitBangs`, `compileUnion`, `checkLength`, `afterReadError`, `readEachEntry` | `packages/globstar-walk/src/walk.js`  |
| `linkToDescend`, `realpath`, `opensAsAnother`, `listed`, `win32Rewrites`, `withSep`               | `packages/globstar-walk/src/walk.js`   |
