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
//! ```ignore
//! use globstar_walk::Walk;
//!
//! for entry in Walk::new("**/*.rs", ".")? {
//!     match entry {
//!         Ok(path) => println!("{}", path.display()),
//!         Err(e)   => eprintln!("walker error: {e}"),
//!     }
//! }
//! # Ok::<(), globstar_walk::WalkError>(())
//! ```
//!
//! # Output paths
//!
//! Yielded paths are always absolute, rooted at the walker's `base`.
//! `base` is locked to an absolute path at construction (via
//! [`std::path::absolute`]) — pure path manipulation, no syscall, no
//! symlink resolution. Subsequent `std::env::set_current_dir` calls do
//! NOT redirect the walk.
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
//! # Separator normalization
//!
//! All matching treats `/` and `\` as equivalent path separators. The
//! walker always builds relative paths using `/` internally, so matching
//! is consistent across platforms.

#![forbid(unsafe_code)]

pub mod entry;
pub mod error;
pub mod options;

pub use entry::DirEntry;
pub use error::WalkError;
pub use options::WalkOptions;

use std::fs;
use std::path;
use std::path::{Path, PathBuf};

use globstar::{CompileOptions, Glob};

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

    /// LIFO stack of directories seen but not yet expanded.
    stack: Vec<Frame>,
    /// Buffer of entries from the most recently expanded directory that
    /// are ready to yield. Drained before the next frame is expanded.
    ready: Vec<Result<DirEntry, WalkError>>,
}

#[derive(Debug)]
struct Frame {
    /// Absolute (or user-provided) path to read via `fs::read_dir`.
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
    /// Empty for non-symlink descents; cloned on each descent (Vec
    /// clone of an empty vec doesn't allocate).
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
    /// (`\!foo` or `[!]foo`) per GLOB_SPEC §9.4.
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
        // clean pattern. Matches the JS walker's `walker/walk.js`.
        let mut positives: Vec<String> = Vec::new();
        let mut negatives: Vec<String> = Vec::new();
        for p in patterns {
            let s = p.as_ref();
            let bangs = s.bytes().take_while(|&b| b == b'!').count();
            let body = if bangs == 0 { s } else { &s[bangs..] };
            if bangs & 1 == 1 {
                negatives.push(body.to_string());
            } else {
                positives.push(body.to_string());
            }
        }

        let matcher = compile_union(positives.iter().map(|s| s.as_str()), compile_opts)?;
        // `opts.ignore` is the user-explicit ignore list; auto-split
        // negatives append to it.
        let ignore = compile_union(
            opts.ignore
                .iter()
                .map(|s| s.as_str())
                .chain(negatives.iter().map(|s| s.as_str())),
            compile_opts,
        )?;
        // Lock the base to an absolute path at construction. `path::absolute`
        // resolves against the current CWD without touching the filesystem and
        // without resolving symlinks — so later `set_current_dir` calls don't
        // redirect this walker, and user-intended symlink bases are preserved.
        let base = path::absolute(&opts.base).map_err(|source| WalkError::Io {
            path: opts.base.clone(),
            source,
        })?;
        let mut w = Self {
            matcher,
            ignore,
            base,
            follow_links: opts.follow_links,
            stack: Vec::new(),
            ready: Vec::new(),
        };
        w.init_from_prefixes();
        Ok(w)
    }

    /// The absolute base directory the walker is rooted at — locked at
    /// construction via [`std::path::absolute`] so subsequent
    /// [`std::env::set_current_dir`] calls don't redirect the walk.
    ///
    /// Yielded [`DirEntry::path`]s are absolute (always rooted at this
    /// `base`). Combine with [`Path::strip_prefix`] to get a path
    /// relative to the walk root:
    ///
    /// ```ignore
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
        let matcher = match self.matcher.as_ref() {
            Some(m) => m,
            None => return, // empty pattern set: yield nothing
        };
        let prefixes = matcher.static_prefixes();
        for prefix in prefixes {
            if prefix.is_empty() {
                if let Ok(meta) = fs::metadata(&self.base) {
                    if meta.is_dir() {
                        self.stack.push(Frame {
                            absolute: self.base.clone(),
                            relative: Vec::new(),
                            depth: 0,
                            symlink_ancestors: Vec::new(),
                        });
                    }
                }
                continue;
            }

            let absolute = match std::str::from_utf8(&prefix) {
                Ok(s) => {
                    // Defense-in-depth: `compile_set` already strips
                    // Unix-style leading `/`, but on Windows a prefix
                    // like `C:/foo` or `\\server\share` would still
                    // make `Path::join` replace `self.base`. Reject
                    // any join result that escapes the base.
                    let joined = self.base.join(s);
                    if joined.strip_prefix(&self.base).is_err() {
                        continue;
                    }
                    joined
                }
                Err(_) => continue,
            };

            // Depth of the prefix from the base: one per segment.
            let depth = 1 + prefix.iter().filter(|&&b| b == b'/').count();

            match fs::metadata(&absolute) {
                Ok(meta) if meta.is_dir() => {
                    let dm = matcher.match_dir(&prefix);
                    if dm.is_match() {
                        self.ready
                            .push(Ok(self.make_entry(&absolute, &meta, depth)));
                    }
                    if dm.should_descend() {
                        self.stack.push(Frame {
                            absolute,
                            relative: prefix,
                            depth,
                            symlink_ancestors: Vec::new(),
                        });
                    }
                }
                Ok(meta) => {
                    if matcher.is_match(&prefix) {
                        self.ready
                            .push(Ok(self.make_entry(&absolute, &meta, depth)));
                    }
                }
                Err(_) => {
                    // Missing prefix target — silently skip. Other
                    // brace-alternative prefixes may still succeed.
                }
            }
        }
    }

    /// Build a [`DirEntry`] from an already-stat'd prefix path. The
    /// init-from-prefixes path has follow-link semantics (we use
    /// `fs::metadata`, not `symlink_metadata`, to decide dir-vs-file),
    /// so the cached metadata here also follows symlinks — different
    /// from entries produced by [`Self::expand_next_frame`], which
    /// cache symlink-preserving metadata. This asymmetry is an artifact
    /// of how glob prefixes are resolved; it only matters when a
    /// top-level literal prefix is itself a symlink.
    fn make_entry(&self, absolute: &Path, meta: &fs::Metadata, depth: usize) -> DirEntry {
        DirEntry {
            path: absolute.to_path_buf(),
            file_type: meta.file_type(),
            depth,
            #[cfg(windows)]
            metadata: meta.clone(),
        }
    }

    fn expand_next_frame(&mut self) {
        let frame = match self.stack.pop() {
            Some(f) => f,
            None => return,
        };

        let entries = match fs::read_dir(&frame.absolute) {
            Ok(e) => e,
            Err(source) => {
                self.ready.push(Err(WalkError::Io {
                    path: frame.absolute,
                    source,
                }));
                return;
            }
        };

        let child_depth = frame.depth + 1;
        let mut new_frames: Vec<Frame> = Vec::new();
        let mut dir_results: Vec<Result<DirEntry, WalkError>> = Vec::new();

        // Hoist matcher / ignore deref out of the per-entry loop. Frames
        // never reach here when `matcher` is `None` (init_from_prefixes
        // returns early in that case), so the unwrap is invariant.
        let matcher = self
            .matcher
            .as_ref()
            .expect("matcher present when stack non-empty");
        let ignore = self.ignore.as_ref();

        for entry_result in entries {
            let entry = match entry_result {
                Ok(e) => e,
                Err(source) => {
                    dir_results.push(Err(WalkError::Io {
                        path: frame.absolute.clone(),
                        source,
                    }));
                    continue;
                }
            };

            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let name_bytes = name_str.as_bytes();

            let mut child_rel = Vec::with_capacity(frame.relative.len() + 1 + name_bytes.len());
            child_rel.extend_from_slice(&frame.relative);
            if !frame.relative.is_empty() {
                child_rel.push(b'/');
            }
            child_rel.extend_from_slice(name_bytes);

            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(source) => {
                    dir_results.push(Err(WalkError::Io {
                        path: entry.path(),
                        source,
                    }));
                    continue;
                }
            };

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
                    dir_results.push(Err(WalkError::Io {
                        path: entry.path(),
                        source,
                    }));
                    continue;
                }
            };

            // `follow_links: false` → symlinks are dropped entirely
            // (matches `tinyglobby`'s `excludeSymlinks` semantics, NOT
            // walkdir's "treat as file"). Skip before any further work.
            if file_type.is_symlink() && !self.follow_links {
                continue;
            }

            let is_dir = if file_type.is_symlink() {
                fs::metadata(entry.path())
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            } else {
                file_type.is_dir()
            };

            if let Some(ig) = ignore {
                if ig.is_match(&child_rel) {
                    continue;
                }
            }

            let make_entry = |path: PathBuf| DirEntry {
                path,
                file_type,
                depth: child_depth,
                #[cfg(windows)]
                metadata: cached_metadata.clone(),
            };

            if is_dir {
                let dm = matcher.match_dir(&child_rel);
                if dm.is_match() {
                    dir_results.push(Ok(make_entry(entry.path())));
                }
                if dm.should_descend() {
                    // Cycle break: when descending through a symlink
                    // target, canonicalize and check against the
                    // ancestor chain. Already-seen target → skip
                    // descent (still emitted above if it matched).
                    let new_ancestors = if file_type.is_symlink() {
                        let resolved = match fs::canonicalize(entry.path()) {
                            Ok(r) => r,
                            Err(_) => continue, // broken / inaccessible
                        };
                        if frame.symlink_ancestors.contains(&resolved) {
                            continue; // cycle
                        }
                        let mut chain = frame.symlink_ancestors.clone();
                        chain.push(resolved);
                        chain
                    } else {
                        frame.symlink_ancestors.clone()
                    };
                    new_frames.push(Frame {
                        absolute: entry.path(),
                        relative: child_rel,
                        depth: child_depth,
                        symlink_ancestors: new_ancestors,
                    });
                }
            } else if matcher.is_match(&child_rel) {
                dir_results.push(Ok(make_entry(entry.path())));
            }
        }

        // Populate `ready` in forward order (pop drains in reverse).
        dir_results.reverse();
        self.ready.extend(dir_results);

        // Push subdirectories in reverse so the first one is processed
        // next (LIFO → DFS with forward order per level).
        for frame in new_frames.into_iter().rev() {
            self.stack.push(frame);
        }
    }
}

impl Iterator for Walk {
    type Item = Result<DirEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.ready.pop() {
                return Some(item);
            }
            if self.stack.is_empty() {
                return None;
            }
            self.expand_next_frame();
        }
    }
}

/// Compile a sequence of patterns into a single brace-merged
/// [`Glob`] via [`Glob::union_with`]. Returns `None` for an empty
/// input list.
///
/// A per-pattern compile failure becomes [`WalkError::InvalidPattern`]
/// with the offending pattern attached.
///
/// Leading `/` is stripped up front so `Glob::static_prefixes()`
/// always returns relative bytes — prevents `Path::join`'s
/// "absolute-replaces-base" behavior from escaping the sandbox.
///
/// Single-pattern inputs bypass `Glob::union_with` (and its internal
/// `Vec<String>` collect) and call `Glob::new_with` directly so the
/// hot path of `Walk::new("**/*.ts")` allocates no scratch vector.
fn compile_union<'a, I>(patterns: I, opts: CompileOptions) -> Result<Option<Glob>, WalkError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut iter = patterns.into_iter();
    let first = match iter.next() {
        Some(s) => s,
        None => return Ok(None),
    };
    let invalid = |pattern: String, e: globstar::GlobError| WalkError::InvalidPattern {
        pattern,
        reason: e.to_string(),
    };
    let Some(second) = iter.next() else {
        let s = first.trim_start_matches('/');
        return Glob::new_with(s, opts)
            .map(Some)
            .map_err(|e| invalid(s.to_string(), e));
    };
    let mut all: Vec<String> = Vec::with_capacity(2);
    all.push(first.trim_start_matches('/').to_string());
    all.push(second.trim_start_matches('/').to_string());
    for s in iter {
        all.push(s.trim_start_matches('/').to_string());
    }
    Glob::union_with(&all, opts)
        .map(Some)
        .map_err(|e| invalid(all.join(","), e))
}
