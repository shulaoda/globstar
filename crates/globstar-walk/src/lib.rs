//! Filesystem walker built on top of [`globstar`].
//!
//! The walker matches a glob (or a union of globs) against a directory
//! tree rooted at `base`. It uses [`globstar`]'s `match_dir` pruning to
//! skip whole subtrees that cannot contain matches, and `static_prefixes`
//! to jump directly into the deepest pre-determined directory (so
//! `src/**/*.ts` never scans the top-level tree).
//!
//! # Example
//!
//! ```no_run
//! use globstar_walk::Walk;
//!
//! for entry in Walk::new("**/*.rs", ".")? {
//!     match entry {
//!         Ok(entry) => println!("{}", entry.path().display()),
//!         Err(e) => eprintln!("walker error: {e}"),
//!     }
//! }
//! # Ok::<(), globstar_walk::WalkError>(())
//! ```
//!
//! # Output paths
//!
//! Yielded paths are always absolute, rooted at the walker's `base`.
//! `base` is locked to an absolute path at construction (via
//! [`std::path::absolute`], with `.` and `..` then taken out as text) —
//! pure path manipulation, no syscall, no symlink resolution.
//! Subsequent `std::env::set_current_dir` calls do NOT redirect the
//! walk.
//!
//! Unlike `tinyglobby` / `fast-glob` (Node.js), there is no `absolute`
//! toggle for relative output. Callers that want paths relative to the
//! walk root can [`Path::strip_prefix`] the output against
//! [`Walk::base`] — that's a one-liner at the consumer boundary, and
//! keeps [`DirEntry::metadata`] working uniformly (always queried on
//! the absolute form). See [`Walk::base`] for an example.
//!
//! # Traversal order
//!
//! Depth-first, with files within a directory yielded before descending
//! into subdirectories. Order within a single directory is whatever
//! `std::fs::read_dir` returns (OS-dependent) — no sorting is imposed.
//!
//! # Symlinks
//!
//! By default (`follow_links: true`, matching `tinyglobby`), symlinks
//! are followed: the target is `stat`-ed, and if it's a directory the
//! walker descends into it. Cycles are broken by canonicalizing each
//! symlink-target and checking against the ancestor chain — the
//! offending descent is dropped, the rest of the walk continues.
//!
//! Set `WalkOptions::follow_links = false` to drop symlinks entirely:
//! they are NEITHER emitted NOR descended (matches `fdir`'s
//! `excludeSymlinks` mode).
//!
//! # Path encoding
//!
//! File names are converted via `to_string_lossy()` before matching,
//! which is free on valid UTF-8 and replaces any non-UTF-8 bytes with
//! U+FFFD. Non-UTF-8 paths (rare, Unix-only) may therefore not match
//! exactly.
//!
//! # Separators
//!
//! Patterns use `/` exclusively (GLOB_SPEC §2.2). In matched paths `/`
//! is always a separator, and on Windows `\` is one too — the
//! separator set follows [`std::path::is_separator`]. The walker
//! builds relative paths with `/` internally, so matching behaves the
//! same across platforms. Yielded paths join `base` and the names
//! `read_dir` gives with the platform's separator (`\` on Windows), so
//! one file has one spelling whatever the pattern's shape.
//!
//! On Windows a seed (a pattern's literal head) stops before a part
//! Win32 would open as something else — one ending in `.` or a space, a
//! device name such as `NUL` or `con.txt` — and the walk starts from its
//! parent, where `read_dir` decides; a part with `:` (a drive letter, a
//! stream) is never below `base`, so `C:/proj/src/*.ts` matches nothing,
//! as on other platforms. Patterns are relative to `base`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod entry;
mod error;
mod options;

pub use entry::DirEntry;
pub use error::WalkError;
pub use options::WalkOptions;

use std::fs;
use std::io;
use std::path;
use std::path::{Path, PathBuf};

use globstar::{CompileOptions, Glob, GlobError};

/// An iterator over paths under a base directory that match one or more
/// glob patterns.
///
/// Construct via [`Walk::new`] (single pattern) or
/// [`Walk::from_patterns`] (union). Both accept anything convertible to
/// [`WalkOptions`] — a path string, a `PathBuf`, or the struct itself.
#[derive(Debug)]
pub struct Walk {
    /// Compiled positive-pattern union. `None` if no positive patterns
    /// were given (walker yields nothing).
    matcher: Option<Glob>,

    /// Compiled ignore-pattern union (combines `opts.ignore` and any
    /// `!`-prefixed patterns auto-split out of the input). `None` if
    /// nothing should be ignored.
    ignore: Option<Glob>,

    base: PathBuf,
    follow_links: bool,
    /// Whether the ignore set can cover a whole subtree (`match_dir`
    /// all-below), which `dot = false` never does.
    prune_ignored: bool,
    /// Whether the seeds are laid out yet: on the first `next()`, so
    /// they see the same tree the walk does.
    seeded: bool,

    /// LIFO stack of directories seen but not yet read.
    stack: Vec<Frame>,

    /// The folder being read, one entry per `next()`.
    reading: Option<Reading>,

    /// The seeds' entries and errors, yielded before any folder is
    /// read (popped, so stored in reverse).
    ready: Vec<Result<DirEntry, WalkError>>,
}

/// A folder being read: its entries stream out as `read_dir` yields
/// them, and its child folders wait until it is done, so the walk stays
/// depth-first with each folder's own entries first. Memory follows the
/// folders found, not the entries read.
#[derive(Debug)]
struct Reading {
    frame: Frame,
    entries: fs::ReadDir,
    /// Child folders to walk, in reading order.
    children: Vec<Frame>,
}

#[derive(Debug)]
struct Frame {
    /// Absolute path to read via `fs::read_dir`.
    absolute: PathBuf,

    /// Relative path from the walker's base directory, encoded with `/`
    /// separators. Empty for the base directory itself.
    relative: Vec<u8>,

    /// Depth of this frame relative to `base`. The base frame is depth 0;
    /// each level of descent adds 1. Cached in the frame so yielded
    /// [`DirEntry`]s don't have to recompute from path bytes.
    depth: usize,

    /// Canonicalized targets of every symlink we descended through to
    /// reach this frame. A new symlink-to-dir descent canonicalizes its
    /// target and skips the descent if the target is already in this
    /// chain — that's our cycle break (matches `fdir`'s `isRecursive`).
    /// Inherited unchanged by a non-symlink descent; a seed starts with
    /// the links on its prefix.
    symlink_ancestors: Vec<PathBuf>,
}

impl Walk {
    /// Create a walker for a single pattern with the given options.
    pub fn new(pattern: &str, opts: impl Into<WalkOptions>) -> Result<Self, WalkError> {
        Self::from_patterns([pattern], opts)
    }

    /// Create a walker for the union of multiple patterns with the given
    /// options. An empty pattern list is accepted and yields nothing.
    ///
    /// **Leading `/` in any input pattern is stripped**, not treated as
    /// an absolute filesystem path. `opts.base` is always the scope
    /// boundary — a pattern like `/a/b/*.ts` walks `base/a/b`, never the
    /// filesystem's `/a/b`. This prevents callers that forward user input
    /// as patterns from accidentally letting the walker escape the
    /// sandbox. If you really want to walk an absolute directory, pass
    /// it as `base`.
    ///
    /// **`!`-prefixed patterns are auto-split** out of `patterns` by
    /// leading-`!` parity (GLOB_SPEC §9.5): odd count routes the body
    /// (with all leading `!`s stripped) into the ignore set, even count
    /// routes it into the positive set. `["**/*.ts", "!**/*.test.ts"]`
    /// is equivalent to `patterns: ["**/*.ts"]` + `ignore: ["**/*.test.ts"]`.
    /// To match a filename literally starting with `!`, escape it
    /// (`\!foo` or `[\!]foo`) per GLOB_SPEC §9.4.
    pub fn from_patterns<I, S>(patterns: I, opts: impl Into<WalkOptions>) -> Result<Self, WalkError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let opts = opts.into();
        let compile_opts = CompileOptions::default()
            .dot(opts.dot)
            .case_insensitive(opts.case_insensitive);

        // Partition input by leading-`!` parity (GLOB_SPEC §9.5): count
        // all leading `!`s, route by parity (odd → ignore, even → match),
        // strip every leading `!` from the body so the parser sees a
        // clean pattern. Matches the JS walker (`packages/globstar-walk/src/walk.js`).
        // Each body keeps the text it came from, for error messages.
        let mut positives: Vec<(String, String)> = Vec::new();
        let mut negatives: Vec<(String, String)> = Vec::new();
        for p in patterns {
            let raw = p.as_ref();
            let Some((bangs, body)) = split_bangs(raw) else {
                continue; // `.`, `//`, `!.` — selects or ignores nothing
            };
            let entry = (raw.to_string(), body);
            if bangs & 1 == 1 {
                negatives.push(entry);
            } else {
                positives.push(entry);
            }
        }

        let matcher = compile_union(&positives, compile_opts)?;
        // `opts.ignore` entries are normalized but never `!`-split
        // (§3.2); auto-split negatives append to them. A leading `!`
        // there would ignore everything else, and is refused.
        let mut ignore_patterns: Vec<(String, String)> = Vec::new();
        for s in &opts.ignore {
            let n = normalize_pattern(s);
            if n.is_empty() && !s.is_empty() {
                continue;
            }
            if n.starts_with('!') {
                return Err(WalkError::InvalidPattern {
                    pattern: s.clone(),
                    reason: "an ignore entry cannot start with `!` (it would ignore \
                             everything else); list what to ignore, and write `\\!` \
                             for a name that starts with `!`"
                        .to_string(),
                    source: None,
                });
            }
            ignore_patterns.push((s.clone(), n));
        }
        ignore_patterns.extend(negatives);
        let ignore = compile_union(&ignore_patterns, compile_opts)?;
        // Lock the base to an absolute path at construction, resolved the
        // way Node's `path.resolve` does: against the current CWD (the
        // empty base is the CWD itself), with `.` and `..` taken out as
        // text, without touching the filesystem or resolving symlinks — so
        // later `set_current_dir` calls don't redirect this walker, and a
        // symlink base stays as given.
        let given: &Path = if opts.base.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &opts.base
        };
        let base = path::absolute(given)
            .map(|p| lexical(&p))
            .map_err(|source| WalkError::Io {
                path: opts.base.clone(),
                source,
            })?;
        Ok(Self {
            matcher,
            ignore,
            base,
            follow_links: opts.follow_links,
            prune_ignored: opts.dot,
            seeded: false,
            stack: Vec::new(),
            reading: None,
            ready: Vec::new(),
        })
    }

    /// The absolute base directory the walker is rooted at — locked at
    /// construction (via [`std::path::absolute`], then `.` and `..` taken
    /// out as text, see [`WalkOptions::base`]) so subsequent
    /// [`std::env::set_current_dir`] calls don't redirect the walk.
    ///
    /// Yielded [`DirEntry::path`]s are absolute (always rooted at this
    /// `base`). Combine with [`Path::strip_prefix`] to get a path
    /// relative to the walk root:
    ///
    /// ```no_run
    /// use globstar_walk::Walk;
    ///
    /// let walker = Walk::new("**/*.rs", "./src")?;
    /// let base = walker.base().to_path_buf();
    /// for entry in walker {
    ///     let p = entry?.into_path();
    ///     let rel = p.strip_prefix(&base).unwrap();
    ///     println!("{}", rel.display());
    /// }
    /// # Ok::<(), globstar_walk::WalkError>(())
    /// ```
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Seed the traversal stack via the matcher's `static_prefixes()`.
    ///
    /// Pure-literal patterns whose prefix resolves to an existing file
    /// are yielded directly (no traversal). Other prefixes become stack
    /// frames under `base`. Missing prefixes silently skip.
    fn init_from_prefixes(&mut self) {
        // A bad base fails loudly whatever the patterns are, never as a
        // silently empty walk.
        let base_error = match fs::metadata(&self.base) {
            Ok(meta) if meta.is_dir() => None,
            Ok(_) => Some(io::Error::new(
                io::ErrorKind::NotADirectory,
                "base is not a directory",
            )),
            Err(err) => Some(err),
        };
        if let Some(source) = base_error {
            self.ready.push(Err(WalkError::Io {
                path: self.base.clone(),
                source,
            }));
            return;
        }
        let matcher = match self.matcher.as_ref() {
            Some(m) => m,
            None => return, // empty pattern set: yield nothing
        };
        let mut prefixes = matcher.static_prefixes();
        if cfg!(windows) {
            prefixes = win32_seeds(prefixes);
        }
        for prefix in prefixes {
            if prefix.is_empty() {
                self.stack.push(Frame {
                    absolute: self.base.clone(),
                    relative: Vec::new(),
                    depth: 0,
                    symlink_ancestors: Vec::new(),
                });
                continue;
            }

            // A walked path never holds `..`, `.` or an empty segment
            // (`read_dir` yields none of them), so a prefix with one
            // (`{a,/b}` gives `/b`) is reached by no root walk; `..` would
            // also escape `base`. Split on every platform separator so a
            // Windows `..\secret` can't slip past.
            if prefix
                .split(|&b| std::path::is_separator(b as char))
                .any(|seg| seg.is_empty() || seg == b".." || seg == b".")
            {
                continue;
            }

            if let Some(ig) = &self.ignore
                && seed_ignored(ig, &prefix)
            {
                continue;
            }

            if !listed_as_is(&self.base, &prefix) {
                continue;
            }

            let absolute = match std::str::from_utf8(&prefix) {
                Ok(s) => {
                    // Part by part, so the path has the platform's
                    // separator throughout, as a root walk's do.
                    let mut joined = self.base.clone();
                    for seg in s.split('/') {
                        joined.push(seg);
                    }
                    // Defense-in-depth: a part that replaces the base
                    // (a drive, a root) never gets past `win32_seeds`.
                    if !joined.starts_with(&self.base) {
                        continue;
                    }
                    joined
                }
                Err(_) => continue,
            };

            // The links a root walk would follow on the way down.
            let Some((symlink_ancestors, descend)) = self.seed_links(&absolute) else {
                continue;
            };

            // Depth of the prefix from the base: one per segment.
            let depth = 1 + prefix
                .iter()
                .filter(|&&b| std::path::is_separator(b as char))
                .count();

            match fs::metadata(&absolute) {
                Ok(meta) if meta.is_dir() => {
                    let dm = matcher.match_dir(&prefix);
                    if dm.matches() {
                        // Entries carry symlink-preserving file_type.
                        if let Ok(lmeta) = fs::symlink_metadata(&absolute) {
                            self.ready
                                .push(Ok(Self::make_entry(&absolute, &lmeta, depth)));
                        }
                    }
                    if descend && dm.may_match_below() {
                        self.stack.push(Frame {
                            absolute,
                            relative: prefix,
                            depth,
                            symlink_ancestors,
                        });
                    }
                }
                Ok(_) => {
                    if matcher.is_match(&prefix)
                        && let Ok(lmeta) = fs::symlink_metadata(&absolute)
                    {
                        self.ready
                            .push(Ok(Self::make_entry(&absolute, &lmeta, depth)));
                    }
                }
                Err(_) => match fs::symlink_metadata(&absolute) {
                    // Still lstat-able ⇒ the entry exists (broken or
                    // looping symlink): the walk emits those as
                    // non-directories (§10), so the seed must too.
                    Ok(lmeta) => {
                        if matcher.is_match(&prefix) {
                            self.ready
                                .push(Ok(Self::make_entry(&absolute, &lmeta, depth)));
                        }
                    }
                    Err(lerr) if nothing_there(&lerr) => {} // §9.4
                    // EACCES etc.: a root walk fails loudly here.
                    Err(lerr) => self.ready.push(Err(WalkError::Io {
                        path: absolute,
                        source: lerr,
                    })),
                },
            }
        }

        // Buffers drain via pop; reverse so the first prefix walks first
        // (§7.2 step 1).
        self.ready.reverse();
        self.stack.reverse();
    }

    /// What a root walk knows on reaching the seed `joined`: the
    /// targets of the symlinks it followed on the way (the frame's
    /// cycle chain, §10), and whether it would descend into the seed;
    /// `None` when it never gets there — a symlink under `follow_links:
    /// false`, which drops them entirely, or a link back to one already
    /// followed. A missing or broken part ends the scan: the seed's own
    /// stat decides.
    fn seed_links(&self, joined: &Path) -> Option<(Vec<PathBuf>, bool)> {
        let rel = joined.strip_prefix(&self.base).ok()?;
        let parts = rel.components().count();
        let mut chain = Vec::new();
        let mut cur = self.base.clone();
        for (i, comp) in rel.components().enumerate() {
            cur.push(comp);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => {}
                Ok(_) => continue,
                Err(_) => break,
            }
            if !self.follow_links {
                return None;
            }
            let Ok(target) = fs::canonicalize(&cur) else {
                break;
            };
            if chain.contains(&target) {
                // A cycle: the walk stops descending here, so it reaches
                // the seed only when the seed is this link.
                return (i + 1 == parts).then_some((chain, false));
            }
            chain.push(target);
        }
        Some((chain, true))
    }

    /// Build a [`DirEntry`] from symlink-preserving metadata (the
    /// canonical `file_type` flavor for every yielded entry).
    fn make_entry(absolute: &Path, meta: &fs::Metadata, depth: usize) -> DirEntry {
        DirEntry {
            path: absolute.to_path_buf(),
            file_type: meta.file_type(),
            depth,
            #[cfg(windows)]
            metadata: meta.clone(),
        }
    }

    /// The next item of the folder being read; `None` once it is done,
    /// when its child folders go on the stack, first child on top.
    fn read_next(&mut self) -> Option<Result<DirEntry, WalkError>> {
        let reading = self.reading.as_mut()?;
        // Frames exist only when `matcher` is `Some` (`init_from_prefixes`
        // returns early otherwise).
        let matcher = self
            .matcher
            .as_ref()
            .expect("matcher present while reading");
        let ignore = self.ignore.as_ref();
        let rules = Rules {
            matcher,
            ignore,
            follow_links: self.follow_links,
            prune_ignored: self.prune_ignored,
        };
        for entry in reading.entries.by_ref() {
            if let Some(item) = rules.visit(entry, &reading.frame, &mut reading.children) {
                return Some(item);
            }
        }
        let done = self.reading.take()?;
        self.stack.extend(done.children.into_iter().rev());
        None
    }
}

/// What decides each entry of a folder.
struct Rules<'a> {
    matcher: &'a Glob,
    ignore: Option<&'a Glob>,
    follow_links: bool,
    prune_ignored: bool,
}

impl Rules<'_> {
    /// One entry of the folder `frame`: the item it yields, if any. A
    /// folder to walk goes to `children`.
    fn visit(
        &self,
        entry: io::Result<fs::DirEntry>,
        frame: &Frame,
        children: &mut Vec<Frame>,
    ) -> Option<Result<DirEntry, WalkError>> {
        let entry = match entry {
            Ok(e) => e,
            Err(source) => {
                return Some(Err(WalkError::Io {
                    path: frame.absolute.clone(),
                    source,
                }));
            }
        };

        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(source) => {
                return Some(Err(WalkError::Io {
                    path: entry.path(),
                    source,
                }));
            }
        };

        // `follow_links: false` → symlinks are dropped entirely
        // (matches `tinyglobby`'s `excludeSymlinks` semantics, NOT
        // walkdir's "treat as file"). Skip before any further work.
        if file_type.is_symlink() && !self.follow_links {
            return None;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let name_bytes = name_str.as_bytes();

        let mut child_rel = Vec::with_capacity(frame.relative.len() + 1 + name_bytes.len());
        child_rel.extend_from_slice(&frame.relative);
        if !frame.relative.is_empty() {
            child_rel.push(b'/');
        }
        child_rel.extend_from_slice(name_bytes);

        // Ignore-filter before any stat, so an ignored symlink never
        // costs a metadata resolution (WALKER_SPEC §7.1 order).
        if let Some(ig) = self.ignore
            && ig.is_match(&child_rel)
        {
            return None;
        }

        // On Windows, `std::fs::DirEntry::metadata` is free — the data
        // was already pulled from `FindFirstFileW` during readdir.
        // Cache it so `DirEntry::metadata` is also syscall-free later.
        // On Unix we skip this (metadata would cost one stat syscall
        // per entry even if the user never asks for it) and fall back
        // to lazy `symlink_metadata` on demand.
        #[cfg(windows)]
        let cached_metadata = match entry.metadata() {
            Ok(m) => m,
            Err(source) => {
                return Some(Err(WalkError::Io {
                    path: entry.path(),
                    source,
                }));
            }
        };

        let is_dir = if file_type.is_symlink() {
            fs::metadata(entry.path())
                .map(|m| m.is_dir())
                .unwrap_or(false)
        } else {
            file_type.is_dir()
        };

        let child_depth = frame.depth + 1;
        let make_entry = |path: PathBuf| DirEntry {
            path,
            file_type,
            depth: child_depth,
            #[cfg(windows)]
            metadata: cached_metadata.clone(),
        };

        if !is_dir {
            return self
                .matcher
                .is_match(&child_rel)
                .then(|| Ok(make_entry(entry.path())));
        }
        let dm = self.matcher.match_dir(&child_rel);
        let emitted = dm.matches().then(|| Ok(make_entry(entry.path())));
        // An ignore that covers everything below skips the subtree
        // (`**/node_modules/**` and `node_modules`).
        let covered = self.prune_ignored
            && self
                .ignore
                .is_some_and(|ig| ig.match_dir(&child_rel).matches_all_below());
        if !dm.may_match_below() || covered {
            return emitted;
        }
        // Cycle break: when descending through a symlink target,
        // canonicalize and check against the ancestor chain. An
        // already-seen target, or a broken or unreadable link, skips the
        // descent (still emitted if it matched).
        let symlink_ancestors = if file_type.is_symlink() {
            let Ok(resolved) = fs::canonicalize(entry.path()) else {
                return emitted;
            };
            if frame.symlink_ancestors.contains(&resolved) {
                return emitted;
            }
            let mut chain = frame.symlink_ancestors.clone();
            chain.push(resolved);
            chain
        } else {
            frame.symlink_ancestors.clone()
        };
        children.push(Frame {
            absolute: entry.path(),
            relative: child_rel,
            depth: child_depth,
            symlink_ancestors,
        });
        emitted
    }
}

// After its first `None` the stack and the folder being read stay
// empty, so every later call is `None` too.
impl std::iter::FusedIterator for Walk {}

impl Iterator for Walk {
    type Item = Result<DirEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.seeded {
            self.seeded = true;
            self.init_from_prefixes();
        }
        loop {
            if let Some(item) = self.ready.pop() {
                return Some(item);
            }
            if self.reading.is_some() {
                if let Some(item) = self.read_next() {
                    return Some(item);
                }
                continue;
            }
            let frame = self.stack.pop()?;
            match fs::read_dir(&frame.absolute) {
                Ok(entries) => {
                    self.reading = Some(Reading {
                        frame,
                        entries,
                        children: Vec::new(),
                    });
                }
                Err(source) => {
                    return Some(Err(WalkError::Io {
                        path: frame.absolute,
                        source,
                    }));
                }
            }
        }
    }
}

/// WALKER_SPEC §4.3: spell patterns the way walked paths are spelled —
/// strip leading `/`s, collapse `//`, drop `.` segments. A last `.`
/// names the folder before it, so it becomes a trailing `/` (`d/.` ≡
/// `d/`, never the files `d` would match). `/` in a pattern is always
/// structural (`\/` is a parse error), so textual splitting is safe.
/// Returns `""` when nothing remains (`.`, `./`).
fn normalize_pattern(s: &str) -> String {
    let s = s.trim_start_matches('/');
    if !s.contains('/') {
        return if s == "." {
            String::new()
        } else {
            s.to_string()
        };
    }
    let trailing = s.ends_with('/') || s.ends_with("/.");
    let segs: Vec<&str> = s
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .collect();
    if segs.is_empty() {
        return String::new();
    }
    let mut out = segs.join("/");
    if trailing {
        out.push('/');
    }
    out
}

/// A stat error that means no entry can be at the path, so a root walk
/// would never list one (§9.4): it is missing, a part is not a folder,
/// a link loops, the name is too long, or it holds a NUL.
fn nothing_there(err: &io::Error) -> bool {
    let kind = err.kind();
    matches!(
        kind,
        io::ErrorKind::NotFound
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::InvalidFilename
            | io::ErrorKind::InvalidInput
    )
    // `ErrorKind::FilesystemLoop` cannot be named on stable Rust yet;
    // std maps every platform's loop error to it, and Debug spells the
    // variant.
    || format!("{kind:?}") == "FilesystemLoop"
}

/// An ignored directory prunes its subtree (§7.1 step 3), so a seed is
/// dead if `ignore` matches it or any `/`-bounded ancestor.
fn seed_ignored(ignore: &Glob, prefix: &[u8]) -> bool {
    for i in 0..prefix.len() {
        if std::path::is_separator(prefix[i] as char) && ignore.is_match(&prefix[..i]) {
            return true;
        }
    }
    ignore.is_match(prefix)
}

/// Compile a sequence of patterns into a single brace-merged
/// [`Glob`] via [`Glob::union_with`]. Returns `None` for an empty
/// input list.
///
/// A compile failure becomes [`WalkError::InvalidPattern`] naming the
/// member the union failed in ([`GlobError::InMember`]), with the error
/// for that member; when it ran past the expansion budget the members
/// share (GLOB_SPEC §7.7), the reason says so. An error no one member
/// explains (the union's compile work) names the list, cut after three.
///
/// Patterns arrive as (written, normalized) pairs: the normalized body
/// by [`normalize_pattern`] (leading `/` stripped, so
/// `Glob::static_prefixes()` always returns relative bytes — prevents
/// `Path::join`'s "absolute-replaces-base" behavior from escaping the
/// sandbox), the written text to name in errors. When the two differ,
/// the reason says which text its byte offsets count in.
///
/// Single-pattern inputs bypass `Glob::union_with` and call
/// `Glob::new_with` directly, so `Walk::new("**/*.ts")` allocates no
/// scratch vector.
fn compile_union(
    patterns: &[(String, String)],
    opts: CompileOptions,
) -> Result<Option<Glob>, WalkError> {
    let bodies = || patterns.iter().map(|(_, body)| body);
    let glob = match patterns {
        [] => return Ok(None),
        [(_, single)] => Glob::new_with(single, opts),
        _ => Glob::union_with(bodies(), opts),
    };
    glob.map(Some).map_err(|e| {
        // A union names the member it failed in, with that member's own
        // error; an error no one member explains (the union's compile
        // work) is about the list.
        let (member, source) = match e {
            GlobError::InMember { index, error } => (Some(&patterns[index]), *error),
            e if patterns.len() == 1 => (Some(&patterns[0]), e),
            e => (None, e),
        };
        let Some((raw, body)) = member else {
            return WalkError::InvalidPattern {
                pattern: name_list(patterns),
                reason: source.to_string(),
                source: Some(source),
            };
        };
        let mut reason = source.to_string();
        if matches!(source, GlobError::BraceExpansionTooLarge { .. }) && patterns.len() > 1 {
            reason.push_str(" (the patterns share this limit, and it ran out in this one)");
        }
        if raw != body {
            reason = format!("{reason} (in '{body}', as the walker reads it)");
        }
        WalkError::InvalidPattern {
            pattern: raw.clone(),
            reason,
            source: Some(source),
        }
    })
}

/// A list of patterns as one name, cut after the first few.
fn name_list(patterns: &[(String, String)]) -> String {
    const SHOWN: usize = 3;
    let raws: Vec<&String> = patterns.iter().take(SHOWN).map(|(raw, _)| raw).collect();
    match patterns.len() - raws.len() {
        0 => format!("{raws:?}"),
        more => format!("{raws:?} and {more} more"),
    }
}

/// WALKER_SPEC §4.2: the `!` count and the body of a written pattern;
/// `None` when it selects or ignores nothing (`.`, `//`, `!.`).
/// Normalization runs before the split (§4.3 first, so `/!x` ≡ `!x`)
/// and again after each `!` run, which can expose a `/` and then
/// another run: `!/!x` ≡ `!!x`, the body `x` with two `!`.
fn split_bangs(raw: &str) -> Option<(usize, String)> {
    let mut body = normalize_pattern(raw);
    if body.is_empty() && !raw.is_empty() {
        return None;
    }
    let mut bangs = 0;
    loop {
        let run = body.bytes().take_while(|&b| b == b'!').count();
        if run == 0 {
            return Some((bangs, body));
        }
        bangs += run;
        let tail = &body[run..];
        let next = normalize_pattern(tail);
        if next.is_empty() && !tail.is_empty() {
            return None;
        }
        body = next;
    }
}

/// Does each non-ASCII part of `prefix` appear in its folder's listing as
/// written? A file system that ignores Unicode normalization (APFS) opens
/// an NFC spelling of a name stored NFD, which a root walk, comparing
/// names as listed, never matches. A folder that cannot be read leaves it
/// to the seed's own stat.
fn listed_as_is(base: &Path, prefix: &[u8]) -> bool {
    let mut dir = base.to_path_buf();
    for seg in prefix.split(|&b| b == b'/') {
        if !seg.is_ascii() {
            let Ok(entries) = fs::read_dir(&dir) else {
                return true;
            };
            if !entries
                .flatten()
                .any(|e| e.file_name().as_encoded_bytes() == seg)
            {
                return false;
            }
        }
        match std::str::from_utf8(seg) {
            Ok(name) => dir.push(name),
            Err(_) => return true,
        }
    }
    true
}

/// The seeds a Windows walk can stat. Win32 rewrites some names before
/// it opens them (it drops a trailing `.` or space, and `NUL`, `con.txt`
/// and the other device names name devices), so a seed through one
/// would reach a folder `read_dir` lists under another name, past the
/// ignore rules. Such a part cuts the seed back to its parent, where
/// `read_dir` decides; a part with `:` (a drive, a stream) is never
/// listed at all, so its seed is dropped. Cut seeds can cover others:
/// those are dropped too, as `static_prefixes` drops covered heads.
fn win32_seeds(prefixes: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(prefixes.len());
    let mut cut = false;
    'seeds: for prefix in prefixes {
        let mut kept = 0;
        for seg in prefix.split(|&b| b == b'/') {
            if seg.contains(&b':') {
                cut = true;
                continue 'seeds;
            }
            if win32_rewrites(seg) {
                cut = true;
                break;
            }
            kept += seg.len() + 1;
        }
        let end = kept.min(prefix.len() + 1).saturating_sub(1);
        out.push(prefix[..end].to_vec());
    }
    if !cut {
        return out;
    }
    out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    out.dedup();
    let mut kept: Vec<Vec<u8>> = Vec::with_capacity(out.len());
    for p in out {
        if p.is_empty() {
            return vec![p];
        }
        let covered = kept
            .iter()
            .any(|k| p.len() > k.len() && p[k.len()] == b'/' && p.starts_with(k));
        if !covered {
            kept.push(p);
        }
    }
    kept
}

/// Does Win32 open this name as something else: a trailing `.` or space,
/// or a device name (`NUL`, `com1`, `con.txt`)?
fn win32_rewrites(seg: &[u8]) -> bool {
    if matches!(seg.last(), Some(b'.' | b' ')) {
        return true;
    }
    let stem = seg.split(|&b| b == b'.').next().unwrap_or(seg);
    let stem = stem.trim_ascii_end().to_ascii_uppercase();
    match stem.as_slice() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] => d.is_ascii_digit(),
        _ => false,
    }
}

/// `p` with `.` and `..` parts taken out as text (`..` at the root
/// stays at the root), as Node's `path.resolve` does.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            path::Component::CurDir => {}
            path::Component::ParentDir => {
                out.pop();
            }
            comp => out.push(comp),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{win32_rewrites, win32_seeds};

    fn seeds(prefixes: &[&str]) -> Vec<String> {
        let bytes = prefixes.iter().map(|p| p.as_bytes().to_vec()).collect();
        win32_seeds(bytes)
            .into_iter()
            .map(|p| String::from_utf8(p).unwrap())
            .collect()
    }

    #[test]
    fn win32_rewritten_names() {
        for name in [
            "src.", "src ", "a.ts.", ".. ", "NUL", "nul", "con.txt", "Com1", "LPT9.log", "aux ",
        ] {
            assert!(win32_rewrites(name.as_bytes()), "{name}");
        }
        for name in [
            "src", ".git", "a.ts", "nulls", "console", "com10", "lpt", "con-x",
        ] {
            assert!(!win32_rewrites(name.as_bytes()), "{name}");
        }
    }

    // A part Win32 would rewrite cuts the seed back to its parent; a part
    // with `:` drops it; seeds a cut one covers go too.
    #[test]
    fn win32_seeds_stop_where_win32_would_rewrite() {
        assert_eq!(seeds(&["src/sub", "lib"]), ["src/sub", "lib"]);
        assert_eq!(seeds(&["src./a"]), [""]);
        assert_eq!(seeds(&["src/sub./x", "src/y"]), ["src"]);
        assert_eq!(seeds(&["src/a.ts.", "lib"]), ["lib", "src"]);
        assert_eq!(seeds(&["C:/gsw/proj/src", "lib"]), ["lib"]);
        assert_eq!(seeds(&["nul", "src/con.txt", "src/x"]), [""]);
        assert!(seeds(&["c:"]).is_empty());
    }
}
