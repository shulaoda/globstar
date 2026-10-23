//! Filesystem walker built on top of [`globstar`].
//!
//! The walker matches a glob (or a union of globs) against a directory
//! tree rooted at `base`. It uses [`globstar`]'s `match_dir` pruning to
//! skip whole subtrees that cannot contain matches, and `static_prefixes`
//! to seed the walk at each pattern's literal head (so `src/**/*.ts`
//! never scans the top-level tree).
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
//! # What is yielded
//!
//! Files that match, and also the folders that match (unlike the JS
//! walker, which yields files only): `**` yields `d` and `d/x.txt`.
//! [`DirEntry::is_dir`] tells them apart, a link by its target; filter on
//! `!entry.is_dir()` for what the JS walker yields.
//! FIFOs, sockets and devices are never yielded (a reader would block on
//! a FIFO); a symlink takes its target's type, and one that resolves to
//! nothing (dangling, looping) is yielded like a file when it matches.
//!
//! A pattern or ignore entry that ends in `/` matches folders only:
//! `src/` yields the folder `src`, never a file of that name, and an
//! ignore entry `node_modules/` prunes the folder `node_modules` with
//! everything below it. Patterns are anchored at `base`, so write
//! `**/node_modules/` for any depth.
//!
//! # Output paths
//!
//! Yielded paths are always absolute, rooted at the walker's `base`.
//! `base` is locked to an absolute path at construction (via
//! [`std::path::absolute`], with `.` and `..` then taken out as text):
//! path manipulation, reading the current directory for a relative
//! `base` and nothing else, no symlink resolution.
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
//! walker descends into it. Cycles are broken by remembering, along
//! each way down, the folders reached through a link: on Unix by their
//! `(dev, ino)`, which the `stat` that typed the link gives for free,
//! with a hit confirmed by canonical path (inode numbers are not unique
//! on every file system); on Windows by canonical path. A link to a
//! folder already reached that way is not descended (it is still
//! yielded when it matches), and the rest of the walk continues; a link
//! back to a plain ancestor is walked once more before it is caught.
//!
//! The break bounds each way down, not the walk: a folder that links
//! reach by several routes is read once per route, each route giving
//! paths of its own, so links that lead the same way more than once
//! multiply the work. Fifteen folders, each but the last holding two
//! links to the next, give `**/f` 32,767 paths, and one more folder
//! doubles them. Every walker that follows links answers the same
//! (walkdir, `ignore`, fast-glob, `find -L`). Walk a tree that others
//! control (a cloned repository, an unpacked archive) with
//! `follow_links: false`.
//!
//! Set `WalkOptions::follow_links = false` to drop symlinks entirely:
//! they are NEITHER emitted NOR descended (matches `fdir`'s
//! `excludeSymlinks` mode).
//!
//! # Path encoding
//!
//! A name that is not valid UTF-8 (Linux allows any bytes) is skipped,
//! as the JS walker skips it (WALKER_SPEC §7.1): it is neither yielded
//! nor read as a folder.
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
//! A seed (a pattern's literal head) is reached part by part, and a
//! part whose spelling could open another entry must be in its folder's
//! listing as written: a non-ASCII name (APFS opens an NFC spelling of a
//! name stored NFD), and on Windows a name Win32 rewrites — one ending in
//! `.` or a space, a device name such as `NUL` or `con.txt`, one with a
//! `:` (a drive letter, a stream). Otherwise the seed is dropped, as a
//! root walk never lists such a spelling: `C:/proj/src/*.ts` matches
//! nothing, as on other platforms. Patterns are relative to `base`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod entry;
mod error;
mod options;

pub use entry::DirEntry;
pub use error::WalkError;
pub use options::WalkOptions;

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use globstar::{CompileOptions, Glob, GlobError};

/// An iterator over paths under a base directory that match one or more
/// glob patterns.
///
/// Construct via [`Walk::new`] (single pattern) or
/// [`Walk::from_patterns`] (union). Both accept anything convertible to
/// [`WalkOptions`] — a path string, a `PathBuf`, or the struct itself.
#[derive(Debug)]
pub struct Walk {
    /// What decides each entry; `None` when no pattern selects anything.
    rules: Option<Rules>,

    base: PathBuf,
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

    /// Scratch: the relative path of the entry being judged.
    rel: Vec<u8>,
}

/// A folder being read: its entries stream out as `read_dir` yields
/// them, and its child folders go on the stack as they are found, to be
/// read after it, so the walk stays depth-first with each folder's own
/// entries first. Memory follows the folders found, not the entries
/// read.
#[derive(Debug)]
struct Reading {
    frame: Frame,
    entries: fs::ReadDir,
    /// Where this folder's children start on the stack. They are in
    /// reading order until the folder is done, then turned, so the first
    /// child is read first.
    mark: usize,
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

    /// The symlinks descended through to reach this frame, for the
    /// cycle break (see [`link_to_descend`]). Inherited unchanged by a
    /// plain descent; a seed
    /// starts with the links on its prefix.
    links: Links,
}

/// The symlinks descended through to reach a frame, the nearest first.
/// A frame shares the list of the frame it was found in, so each link is
/// held once, however many folders wait below it.
#[derive(Debug, Clone, Default)]
struct Links(Option<Arc<LinkNode>>);

#[derive(Debug)]
struct LinkNode {
    link: Link,
    up: Links,
}

impl Links {
    fn iter(&self) -> impl Iterator<Item = &Link> {
        std::iter::successors(self.0.as_deref(), |node| node.up.0.as_deref()).map(|node| &node.link)
    }

    fn push(&mut self, link: Link) {
        let up = Links(self.0.take());
        self.0 = Some(Arc::new(LinkNode { link, up }));
    }
}

/// A symlink descended through: the identity of its target, so a link
/// back to it on the way down is found.
#[derive(Debug)]
struct Link {
    /// The target's `(dev, ino)`, from the `stat` that typed the link.
    #[cfg(unix)]
    id: (u64, u64),
    /// The link as walked, to confirm an identity hit by canonical path.
    #[cfg(unix)]
    path: PathBuf,
    /// The target's canonical path.
    #[cfg(not(unix))]
    canonical: PathBuf,
}

impl Walk {
    /// Create a walker for a single pattern with the given options.
    pub fn new(pattern: &str, opts: impl Into<WalkOptions>) -> Result<Self, WalkError> {
        Self::from_patterns([pattern], opts)
    }

    /// Create a walker for the union of multiple patterns with the given
    /// options. An empty pattern list is accepted and yields nothing but
    /// a bad base's `Io` error.
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
    /// leading-`!` parity (GLOB_SPEC §11.1): odd count routes the body
    /// (with all leading `!`s stripped) into the ignore set, even count
    /// routes it into the positive set. `["**/*.ts", "!**/*.test.ts"]`
    /// is equivalent to `patterns: ["**/*.ts"]` + `ignore: ["**/*.test.ts"]`.
    /// To match a filename literally starting with `!`, escape it
    /// (`\!foo` or `[\!]foo`) per GLOB_SPEC §11.3.
    pub fn from_patterns<I, S>(patterns: I, opts: impl Into<WalkOptions>) -> Result<Self, WalkError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let opts = opts.into();
        let compile_opts = CompileOptions::default()
            .dot(opts.dot)
            .case_insensitive(opts.case_insensitive);

        // Partition input by leading-`!` parity (GLOB_SPEC §11.1): count
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
        let dirs = compile_union(&folders_only(&positives), compile_opts)?;
        // `opts.ignore` entries are normalized but never `!`-split
        // (§3.2); auto-split negatives append to them. A leading `!`
        // there would ignore everything else, and is refused.
        let mut ignore_patterns: Vec<(String, String)> = Vec::new();
        for s in &opts.ignore {
            let Some(n) = normalize_pattern(s) else {
                continue;
            };
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
        let rules = matcher.map(|matcher| Rules {
            matcher,
            ignore,
            dirs,
            follow_links: opts.follow_links,
            prune_ignored: opts.dot,
        });
        Ok(Self {
            rules,
            base,
            seeded: false,
            stack: Vec::new(),
            reading: None,
            ready: Vec::new(),
            rel: Vec::new(),
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

    /// Seed the traversal from the matcher's `static_prefixes()`. A seed
    /// is reached the way a root walk reaches it: part by part from
    /// `base`, each part judged by [`Rules::judge`] as its folder's only
    /// entry (its `lstat` stands in for the listing). An ancestor that
    /// gives no child frame drops the seed; the last part's entry and
    /// frame are the seed's.
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
        let Some(rules) = &self.rules else {
            return; // empty pattern set: yield nothing
        };
        for prefix in rules.matcher.static_prefixes() {
            let mut frame = Frame {
                absolute: self.base.clone(),
                relative: Vec::new(),
                depth: 0,
                links: Links::default(),
            };
            if prefix.is_empty() {
                self.stack.push(frame);
                continue;
            }
            let Ok(text) = std::str::from_utf8(&prefix) else {
                continue;
            };
            // A walked path never holds `..`, `.` or an empty segment, so
            // no root walk reaches such a prefix (and `..` would escape
            // `base`). Split on every platform separator, as the JS walker
            // does: a root walk lists `a\b` on Windows as `a`, then `b`.
            let parts: Vec<&str> = text.split(path::is_separator).collect();
            if parts
                .iter()
                .any(|&seg| seg.is_empty() || seg == ".." || seg == ".")
            {
                continue;
            }
            for (i, name) in parts.iter().enumerate() {
                if opens_as_another(name) && !listed(&frame.absolute, name) {
                    break;
                }
                let path = join(&frame.absolute, name.as_ref());
                let meta = match fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(e) if nothing_there(&e) => break, // §9.4
                    Err(source) => {
                        self.ready.push(Err(WalkError::Io { path, source }));
                        break;
                    }
                };
                let listed = Listed {
                    name: name.as_ref(),
                    file_type: meta.file_type(),
                    #[cfg(windows)]
                    metadata: meta,
                };
                let mut below = Vec::new();
                let item = rules.judge(&frame, listed, &mut self.rel, &mut below);
                if i + 1 == parts.len() {
                    self.ready.extend(item.map(Ok));
                    self.stack.extend(below);
                    break;
                }
                match below.pop() {
                    Some(child) => frame = child,
                    None => break,
                }
            }
        }

        // Buffers drain via pop; reverse so the first prefix walks first
        // (§7.2 step 1).
        self.ready.reverse();
        self.stack.reverse();
    }

    /// The next item of the folder being read; `None` once it is done,
    /// when its child folders on the stack are turned, first child on
    /// top.
    fn read_next(&mut self) -> Option<Result<DirEntry, WalkError>> {
        let reading = self.reading.as_mut()?;
        let rules = self.rules.as_ref()?;
        for entry in reading.entries.by_ref() {
            if let Some(item) = rules
                .visit(entry, &reading.frame, &mut self.rel, &mut self.stack)
                .transpose()
            {
                return Some(item);
            }
        }
        let done = self.reading.take()?;
        self.stack[done.mark..].reverse();
        None
    }
}

/// What decides each entry of a folder.
#[derive(Debug)]
struct Rules {
    matcher: Glob,
    /// `opts.ignore` and the `!`-split patterns.
    ignore: Option<Glob>,
    /// The patterns that end in `/`, which match folders only: asked
    /// about a folder with its `/`.
    dirs: Option<Glob>,
    follow_links: bool,
    /// Whether the ignore set can cover a whole subtree (`match_dir`
    /// all-below), which `dot = false` never does.
    prune_ignored: bool,
}

/// An entry as its folder lists it.
struct Listed<'a> {
    name: &'a std::ffi::OsStr,
    /// Symlink-preserving.
    file_type: fs::FileType,
    #[cfg(windows)]
    metadata: fs::Metadata,
}

impl Rules {
    /// One `read_dir` item of the folder `frame`.
    fn visit(
        &self,
        entry: io::Result<fs::DirEntry>,
        frame: &Frame,
        rel: &mut Vec<u8>,
        children: &mut Vec<Frame>,
    ) -> Result<Option<DirEntry>, WalkError> {
        // An error names the folder being read, as the JS walker's does.
        let fail = |source| WalkError::Io {
            path: frame.absolute.clone(),
            source,
        };
        let entry = entry.map_err(fail)?;
        let name = entry.file_name();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            // Typed by its own `lstat` where the listing keeps no types,
            // and gone since the read: as if it had gone before it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(fail(source)),
        };
        let listed = Listed {
            name: &name,
            file_type,
            // Free on Windows: `FindFirstFileW` filled it in.
            #[cfg(windows)]
            metadata: entry.metadata().map_err(fail)?,
        };
        Ok(self.judge(frame, listed, rel, children))
    }

    /// The entry `name` of the folder `frame`: what it yields, if
    /// anything. A folder to walk goes to `children`; `rel` is scratch.
    fn judge(
        &self,
        frame: &Frame,
        listed: Listed,
        rel: &mut Vec<u8>,
        children: &mut Vec<Frame>,
    ) -> Option<DirEntry> {
        let Listed {
            name, file_type, ..
        } = listed;
        // `follow_links: false` drops symlinks entirely.
        if file_type.is_symlink() && !self.follow_links {
            return None;
        }
        // A name that is not UTF-8 is skipped (§7.1): no pattern names it.
        let text = name.to_str()?;
        rel.clear();
        rel.extend_from_slice(&frame.relative);
        if !frame.relative.is_empty() {
            rel.push(b'/');
        }
        rel.extend_from_slice(text.as_bytes());

        // Ignore-filter before any stat, so an ignored symlink never
        // costs a metadata resolution (WALKER_SPEC §7.1 order).
        if self.ignore.as_ref().is_some_and(|ig| ig.is_match(rel)) {
            return None;
        }
        // Built only when needed, as `fs::DirEntry::path` builds it.
        let path = || join(&frame.absolute, name);
        // A symlink takes its target's type; one that resolves to nothing
        // (dangling, looping) stays a link, emitted like a file.
        let target = file_type
            .is_symlink()
            .then(|| fs::metadata(path()).ok())
            .flatten();
        let typed = target.as_ref().map_or(file_type, fs::Metadata::file_type);
        if is_special(typed) {
            return None;
        }
        let depth = frame.depth + 1;
        let entry = |path| DirEntry {
            path,
            file_type,
            is_dir: typed.is_dir(),
            depth,
            #[cfg(windows)]
            metadata: listed.metadata.clone(),
        };
        if !typed.is_dir() {
            return self.matcher.is_match(rel).then(|| entry(path()));
        }
        // Asked with its `/`, the folder is pruned by an ignore entry that
        // ends in `/` or `/**` (§3.2), under either `dot`.
        if folder_matches(self.ignore.as_ref(), rel) {
            return None;
        }
        let dm = self.matcher.match_dir(rel);
        let emitted =
            (dm.matches() || folder_matches(self.dirs.as_ref(), rel)).then(|| entry(path()));
        // An ignore that covers everything below skips the subtree
        // (`**/x/**/*`; one that ends in `/**` was caught above).
        let covered = self.prune_ignored
            && self
                .ignore
                .as_ref()
                .is_some_and(|ig| ig.match_dir(rel).matches_all_below());
        if !dm.may_match_below() || covered {
            return emitted;
        }
        // Cycle break: a link to a folder already on the way down is not
        // descended (still emitted).
        let path = path();
        let mut links = frame.links.clone();
        if let Some(target) = &target {
            match link_to_descend(&links, &path, target) {
                Some(link) => links.push(link),
                None => return emitted,
            }
        }
        children.push(Frame {
            absolute: path,
            relative: rel.clone(),
            depth,
            links,
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
        if let Some(item) = self.ready.pop() {
            return Some(item);
        }
        loop {
            if let Some(item) = self.read_next() {
                return Some(item);
            }
            let frame = self.stack.pop()?;
            match fs::read_dir(&frame.absolute) {
                Ok(entries) => {
                    self.reading = Some(Reading {
                        frame,
                        entries,
                        mark: self.stack.len(),
                    });
                }
                // Listed, then removed or made a file before it was read:
                // as if it had gone before its folder was (§9.3).
                Err(e) if gone(&e) => {}
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
/// `None` when nothing remains (`.`, `./`, `//`): it selects or ignores
/// nothing. The written `""` stays, for the core to refuse.
fn normalize_pattern(raw: &str) -> Option<String> {
    let s = raw.trim_start_matches('/');
    let trailing = s.ends_with('/') || s.ends_with("/.");
    let segs: Vec<&str> = s
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .collect();
    if segs.is_empty() {
        return raw.is_empty().then(String::new);
    }
    let mut out = segs.join("/");
    if trailing {
        out.push('/');
    }
    Some(out)
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

/// A read error that says the folder is no longer there: removed, or
/// replaced by a file. The base was checked first, so it never stands for
/// a missing base.
fn gone(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// The (written, normalized) patterns as one [`Glob`], or `None` for
/// none. A failure names the member as written, with that member's error
/// (and says which text its offsets count in, when normalizing changed
/// it); an error no one member explains names the list, cut after three.
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
/// Normalization runs once, before the split (§4.3 first, so `/!x` ≡
/// `!x`). Cutting a `!` run can leave only its own segment empty or
/// `.`; that segment goes, with its `/`, and a run after it counts too:
/// `!/!x` ≡ `!!x`, the body `x` with two `!`.
fn split_bangs(raw: &str) -> Option<(usize, String)> {
    let mut body = normalize_pattern(raw)?;
    let mut rest = body.as_str();
    let mut bangs = 0;
    loop {
        let run = rest.bytes().take_while(|&b| b == b'!').count();
        if run == 0 {
            let cut = body.len() - rest.len();
            body.drain(..cut);
            return Some((bangs, body));
        }
        bangs += run;
        rest = &rest[run..];
        if rest.is_empty() {
            return Some((bangs, String::new()));
        }
        let left = rest.strip_prefix('.').unwrap_or(rest);
        if left.is_empty() || left.starts_with('/') {
            rest = left.strip_prefix('/').unwrap_or(left);
            if rest.is_empty() {
                return None;
            }
        }
    }
}

/// The entries that end in `/` (WALKER_SPEC §3.2), which match folders
/// only.
fn folders_only(entries: &[(String, String)]) -> Vec<(String, String)> {
    entries
        .iter()
        .filter(|(_, body)| body.ends_with('/'))
        .cloned()
        .collect()
}

/// Does `glob` match the folder `rel`, asked with its `/`? Only a pattern
/// that ends in `/` or `/**` can.
fn folder_matches(glob: Option<&Glob>, rel: &mut Vec<u8>) -> bool {
    glob.is_some_and(|g| {
        rel.push(b'/');
        let hit = g.is_match(rel);
        rel.pop();
        hit
    })
}

/// `dir/name` in one allocation (`Path::join` takes two).
fn join(dir: &Path, name: &OsStr) -> PathBuf {
    let mut path = PathBuf::with_capacity(dir.as_os_str().len() + 1 + name.len());
    path.push(dir);
    path.push(name);
    path
}

/// The link `path`, whose target `target` typed, as a frame's [`Link`];
/// `None` when descending it would close a cycle, its target being on
/// the way down already (`links`). A folder has one `(dev, ino)`, which
/// the `stat` gave for free; a hit is confirmed by canonical path, since
/// some file systems hand out inode numbers that are not unique.
#[cfg(unix)]
fn link_to_descend(links: &Links, path: &Path, target: &fs::Metadata) -> Option<Link> {
    use std::os::unix::fs::MetadataExt;
    let id = (target.dev(), target.ino());
    let mut same = links.iter().filter(|l| l.id == id).peekable();
    if same.peek().is_some() {
        let here = fs::canonicalize(path).ok()?;
        if same.any(|l| fs::canonicalize(&l.path).is_ok_and(|p| p == here)) {
            return None;
        }
    }
    Some(Link {
        id,
        path: path.to_path_buf(),
    })
}

/// Windows has no cheap identity in a `stat`: the canonical path is
/// compared.
#[cfg(not(unix))]
fn link_to_descend(links: &Links, path: &Path, _target: &fs::Metadata) -> Option<Link> {
    let canonical = fs::canonicalize(path).ok()?;
    (!links.iter().any(|l| l.canonical == canonical)).then_some(Link { canonical })
}

/// A FIFO, a socket or a device: not a file to read, so never emitted (a
/// reader would block on a FIFO).
fn is_special(t: fs::FileType) -> bool {
    !t.is_dir() && !t.is_file() && !t.is_symlink()
}

/// Can opening `name` reach an entry its folder lists under another name?
/// A file system that ignores Unicode normalization (APFS) opens an NFC
/// spelling of a name stored NFD; Win32 drops a trailing `.` or space,
/// opens `NUL`, `con.txt` and the other device names as devices, and reads
/// `C:` as a drive. A root walk, comparing names as listed, never matches
/// such a spelling.
fn opens_as_another(name: &str) -> bool {
    !name.is_ascii() || (cfg!(windows) && win32_rewrites(name.as_bytes()))
}

/// Is `name` in the listing of `dir` as written? A folder that cannot be
/// read leaves it to the `lstat`.
fn listed(dir: &Path, name: &str) -> bool {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .any(|e| e.file_name().as_encoded_bytes() == name.as_bytes()),
        Err(_) => true,
    }
}

/// Does Win32 open this name as something else: a trailing `.` or space,
/// a device name (`NUL`, `com1`, `con.txt`), or a `:` (a drive, a stream)?
fn win32_rewrites(seg: &[u8]) -> bool {
    if matches!(seg.last(), Some(b'.' | b' ')) || seg.contains(&b':') {
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
    use super::win32_rewrites;

    #[test]
    fn win32_rewritten_names() {
        for name in [
            "src.", "src ", "a.ts.", ".. ", "NUL", "nul", "con.txt", "Com1", "LPT9.log", "aux ",
            "C:", "a:b",
        ] {
            assert!(win32_rewrites(name.as_bytes()), "{name}");
        }
        for name in [
            "src", ".git", "a.ts", "nulls", "console", "com10", "lpt", "con-x",
        ] {
            assert!(!win32_rewrites(name.as_bytes()), "{name}");
        }
    }
}
