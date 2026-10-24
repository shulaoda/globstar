//! A filesystem walker on [`globstar`]: the paths under a base folder
//! that match a glob, or a union of globs. `match_dir` prunes the
//! subtrees nothing below can match, and `static_prefixes` seeds the
//! walk at each pattern's literal head, so `src/**/*.ts` never reads the
//! top-level tree.
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
//! The files and the folders that match (the JS walker yields files
//! only): `**` yields `d` and `d/x.txt`, and [`DirEntry::is_dir`] tells
//! them apart. FIFOs, sockets and devices are never yielded; a symlink
//! takes its target's type, and one that resolves to nothing (dangling,
//! looping) is yielded like a file when it matches.
//!
//! A pattern or ignore entry that ends in `/` matches folders only:
//! `src/` yields the folder `src`, never a file of that name, and the
//! ignore entry `node_modules/` prunes that folder with everything below
//! it. Patterns are anchored at `base`: write `**/node_modules/` for any
//! depth.
//!
//! # Paths
//!
//! Yielded paths are absolute, under [`Walk::base`], which is fixed at
//! construction ([`std::path::absolute`], then `.` and `..` taken out as
//! text, no symlink resolved); a later [`std::env::set_current_dir`]
//! does not move the walk. For paths relative to the base,
//! [`Path::strip_prefix`] against [`Walk::base`].
//!
//! Patterns use `/` (GLOB_SPEC §2.2). In paths `/` is a separator on
//! every platform and `\` on Windows too; yielded paths join `base` and
//! the names `read_dir` gives with the platform's separator. A name that
//! is not valid UTF-8 is skipped, neither yielded nor read as a folder
//! (WALKER_SPEC §7.1).
//!
//! # Order
//!
//! Depth-first, a folder's files before its subfolders; within a folder,
//! the order `read_dir` gives.
//!
//! # Symlinks
//!
//! Followed by default: the target is `stat`-ed, and a folder is
//! descended. A link to a folder already reached through a link on the
//! way down is not descended (it is still yielded when it matches); a
//! link back to a plain ancestor is walked once more before it is
//! caught. The break bounds each way down, not the walk: a folder that
//! links reach by several routes is read once per route, so links that
//! lead the same way more than once multiply the work (fifteen folders,
//! each but the last holding two links to the next, give `**/f` 32,767
//! paths), as in every walker that follows links. Walk a tree that
//! others control with `follow_links: false`, which drops symlinks
//! entirely: neither yielded nor descended.
//!
//! # Seeds
//!
//! A seed is reached part by part, and a part whose spelling could open
//! another entry must be in its folder's listing as written: a non-ASCII
//! name (APFS opens an NFC spelling of a name stored NFD), and on
//! Windows a name Win32 rewrites (one ending in `.` or a space, a device
//! name such as `NUL` or `con.txt`, one with a `:`). Otherwise the seed
//! is dropped, as a root walk never lists such a spelling.

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

use globstar::error::MAX_PATTERN_LEN;
use globstar::{CompileOptions, Glob, GlobError};

/// An iterator over the paths under a base folder that match one or
/// more glob patterns: [`Walk::new`] for one pattern,
/// [`Walk::from_patterns`] for a union. Both take anything that converts
/// to [`WalkOptions`]: a path string, a `PathBuf`, or the struct itself.
#[derive(Debug)]
pub struct Walk {
    /// `None` when no pattern selects anything.
    rules: Option<Rules>,
    base: PathBuf,
    /// The seeds are laid out on the first `next()`, so they see the
    /// tree the walk does.
    seeded: bool,
    /// Folders found but not read yet, the next on top.
    stack: Vec<Frame>,
    reading: Option<Reading>,
    /// The seeds' entries and errors, popped, so stored in reverse.
    ready: Vec<Result<DirEntry, WalkError>>,
    /// Scratch: the relative path of the entry being judged.
    rel: Vec<u8>,
}

/// A folder being read: its entries stream out as `read_dir` yields
/// them, and its child folders go on the stack to be read after it.
#[derive(Debug)]
struct Reading {
    frame: Frame,
    entries: fs::ReadDir,
    /// Where its children start on the stack: in reading order until the
    /// folder is done, then turned, so the first child is read first.
    mark: usize,
}

#[derive(Debug)]
struct Frame {
    absolute: PathBuf,
    /// From `base`, with `/`; empty for `base` itself.
    relative: Vec<u8>,
    /// `base` is 0.
    depth: usize,
    links: Links,
}

/// The symlinks descended through to reach a frame, the nearest first;
/// shared with the frame it was found in, so each link is held once.
#[derive(Debug, Clone, Default)]
struct Links(Option<Arc<LinkNode>>);

#[derive(Debug)]
struct LinkNode {
    link: Link,
    up: Links,
}

/// A symlink descended through: its target's identity, so a link back
/// to it on the way down is found.
#[derive(Debug)]
struct Link {
    /// The target's `(dev, ino)`, from the `stat` that typed the link.
    #[cfg(unix)]
    id: (u64, u64),
    /// The link as walked, to confirm an identity hit by canonical path.
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(not(unix))]
    canonical: PathBuf,
}

/// What decides each entry of a folder.
#[derive(Debug)]
struct Rules {
    matcher: Glob,
    /// `opts.ignore` and the `!`-split patterns.
    ignore: Option<Glob>,
    /// The patterns that end in `/`: asked about a folder with its `/`.
    dirs: Option<Glob>,
    follow_links: bool,
    /// Whether the ignore set can cover a whole subtree, which
    /// `dot = false` never does.
    prune_ignored: bool,
}

/// An entry as its folder lists it.
struct Listed<'a> {
    name: &'a OsStr,
    /// Symlink-preserving.
    file_type: fs::FileType,
    #[cfg(windows)]
    metadata: fs::Metadata,
}

impl Walk {
    /// A walker for one pattern.
    pub fn new(pattern: &str, opts: impl Into<WalkOptions>) -> Result<Self, WalkError> {
        Self::from_patterns([pattern], opts)
    }

    /// A walker for the union of `patterns`; an empty list yields
    /// nothing but a bad base's `Io` error.
    ///
    /// A leading `/` in a pattern is stripped, never read as an absolute
    /// path: `/a/b/*.ts` walks `base/a/b`, so a caller that forwards
    /// user input cannot be led out of `base`.
    ///
    /// A `!`-prefixed pattern goes to the ignore set by leading-`!`
    /// parity (GLOB_SPEC §11.1): an odd count ignores the body, an even
    /// count selects it, every leading `!` stripped. `["**/*.ts",
    /// "!**/*.test.ts"]` is `patterns: ["**/*.ts"]` with `ignore:
    /// ["**/*.test.ts"]`. A name that starts with `!` is written escaped
    /// (`\!foo`, `[\!]foo`; GLOB_SPEC §11.3).
    pub fn from_patterns<I, S>(patterns: I, opts: impl Into<WalkOptions>) -> Result<Self, WalkError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let opts = opts.into();
        let compile_opts = CompileOptions::default()
            .dot(opts.dot)
            .case_insensitive(opts.case_insensitive);

        // Each body keeps the text it came from, for error messages.
        let mut positives: Vec<(String, String)> = Vec::new();
        let mut negatives: Vec<(String, String)> = Vec::new();
        for pattern in patterns {
            let raw = pattern.as_ref();
            check_length(raw)?;
            let Some((bangs, body)) = split_bangs(raw) else {
                continue;
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
        // An ignore entry is normalized but never `!`-split (WALKER_SPEC
        // §3.2); a leading `!` there would ignore everything else.
        let mut ignore_patterns: Vec<(String, String)> = Vec::new();
        for raw in &opts.ignore {
            check_length(raw)?;
            let Some(body) = normalize_pattern(raw) else {
                continue;
            };
            if body.starts_with('!') {
                return Err(WalkError::InvalidPattern {
                    pattern: raw.clone(),
                    reason: "an ignore entry cannot start with `!` (it would ignore \
                             everything else); list what to ignore, and write `\\!` \
                             for a name that starts with `!`"
                        .to_string(),
                    source: None,
                });
            }
            ignore_patterns.push((raw.clone(), body));
        }
        ignore_patterns.extend(negatives);
        let ignore = compile_union(&ignore_patterns, compile_opts)?;
        // As Node's `path.resolve`: against the current directory, `.`
        // and `..` taken out as text, no symlink resolved.
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

    /// The absolute base folder the walk is rooted at (see
    /// [`WalkOptions::base`]). Yielded paths start with it:
    ///
    /// ```no_run
    /// use globstar_walk::Walk;
    ///
    /// let walker = Walk::new("**/*.rs", "./src")?;
    /// let base = walker.base().to_path_buf();
    /// for entry in walker {
    ///     let p = entry?.into_path();
    ///     println!("{}", p.strip_prefix(&base).unwrap().display());
    /// }
    /// # Ok::<(), globstar_walk::WalkError>(())
    /// ```
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Lay out the seeds. Each is reached the way a root walk reaches it:
    /// part by part from `base`, each part judged as its folder's only
    /// entry (its `lstat` stands in for the listing). An ancestor that
    /// gives no child frame drops the seed.
    fn init_from_prefixes(&mut self) {
        // A bad base fails loudly whatever the patterns are.
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
            return;
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
            // no root walk reaches such a prefix. Split on every platform
            // separator: a root walk lists `a\b` on Windows as `a`, `b`.
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
                    Err(e) if nothing_there(&e) => break,
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
        // Popped: the first seed walks first (WALKER_SPEC §7.2).
        self.ready.reverse();
        self.stack.reverse();
    }

    /// The next item of the folder being read; `None` once it is done,
    /// its children turned on the stack.
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

impl Rules {
    /// One `read_dir` item of the folder `frame`.
    fn visit(
        &self,
        entry: io::Result<fs::DirEntry>,
        frame: &Frame,
        rel: &mut Vec<u8>,
        children: &mut Vec<Frame>,
    ) -> Result<Option<DirEntry>, WalkError> {
        // An error names the folder being read.
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
        if file_type.is_symlink() && !self.follow_links {
            return None;
        }
        // A name that is not UTF-8 is skipped (WALKER_SPEC §7.1).
        let text = name.to_str()?;
        rel.clear();
        rel.extend_from_slice(&frame.relative);
        if !frame.relative.is_empty() {
            rel.push(b'/');
        }
        rel.extend_from_slice(text.as_bytes());

        // Before any stat: an ignored symlink costs no resolution.
        if self.ignore.as_ref().is_some_and(|ig| ig.is_match(rel)) {
            return None;
        }
        let path = || join(&frame.absolute, name);
        // A symlink takes its target's type; one that resolves to nothing
        // stays a link, yielded like a file.
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
        // Asked with its `/`, a folder is pruned by an ignore entry that
        // ends in `/` or `/**` (WALKER_SPEC §3.2), under either `dot`.
        if folder_matches(self.ignore.as_ref(), rel) {
            return None;
        }
        let dir_match = self.matcher.match_dir(rel);
        let emitted =
            (dir_match.matches() || folder_matches(self.dirs.as_ref(), rel)).then(|| entry(path()));
        // An ignore that covers everything below skips the subtree.
        let covered = self.prune_ignored
            && self
                .ignore
                .as_ref()
                .is_some_and(|ig| ig.match_dir(rel).matches_all_below());
        if !dir_match.may_match_below() || covered {
            return emitted;
        }
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

impl Links {
    fn iter(&self) -> impl Iterator<Item = &Link> {
        std::iter::successors(self.0.as_deref(), |node| node.up.0.as_deref()).map(|node| &node.link)
    }

    fn push(&mut self, link: Link) {
        let up = Links(self.0.take());
        self.0 = Some(Arc::new(LinkNode { link, up }));
    }
}

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
                // Removed, or made a file, since its parent listed it: as
                // if it had gone before (WALKER_SPEC §9.3).
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

/// After its first `None` the stack and the folder being read stay
/// empty.
impl std::iter::FusedIterator for Walk {}

/// A pattern spelled the way walked paths are (WALKER_SPEC §4.3):
/// leading `/`s stripped, `//` collapsed, `.` segments dropped, a last
/// `.` kept as a trailing `/` (`d/.` ≡ `d/`). `None` when nothing
/// remains (`.`, `./`, `//`); the written `""` stays, for the core to
/// refuse.
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

/// The `!` count and the body of a written pattern (WALKER_SPEC §4.2);
/// `None` when it selects or ignores nothing (`.`, `//`, `!.`).
/// Normalized first, so `/!x` ≡ `!x`; cutting a `!` run can leave only
/// its own segment empty or `.`, which goes with its `/`, and a run
/// after it counts too (`!/!x` ≡ `!!x`).
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

/// A written pattern longer than [`MAX_PATTERN_LEN`] is refused before
/// it is read (WALKER_SPEC §9.2), named by its first characters.
fn check_length(raw: &str) -> Result<(), WalkError> {
    if raw.len() <= MAX_PATTERN_LEN {
        return Ok(());
    }
    let source = GlobError::TooLong {
        len: raw.len(),
        max: MAX_PATTERN_LEN,
    };
    let head: String = raw.chars().take(64).collect();
    Err(WalkError::InvalidPattern {
        pattern: format!("{head}…"),
        reason: source.to_string(),
        source: Some(source),
    })
}

/// The (written, normalized) patterns as one [`Glob`], or `None` for
/// none. A failure names the member as written, with that member's
/// error, and the text its offsets count in when normalizing changed it.
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
        let (index, source) = match e {
            GlobError::InMember { index, error } => (index, *error),
            e => (0, e),
        };
        let (raw, body) = &patterns[index];
        let mut reason = source.to_string();
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

fn folders_only(entries: &[(String, String)]) -> Vec<(String, String)> {
    entries
        .iter()
        .filter(|(_, body)| body.ends_with('/'))
        .cloned()
        .collect()
}

/// Does `glob` match the folder `rel`, asked with its `/`? Only a
/// pattern that ends in `/` or `/**` can.
fn folder_matches(glob: Option<&Glob>, rel: &mut Vec<u8>) -> bool {
    glob.is_some_and(|g| {
        rel.push(b'/');
        let hit = g.is_match(rel);
        rel.pop();
        hit
    })
}

/// A stat error that means no entry can be at the path, so a root walk
/// would never list one (WALKER_SPEC §9.4).
fn nothing_there(err: &io::Error) -> bool {
    let kind = err.kind();
    matches!(
        kind,
        io::ErrorKind::NotFound
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::InvalidFilename
            | io::ErrorKind::InvalidInput
    )
    // `ErrorKind::FilesystemLoop` cannot be named on stable Rust yet.
    || format!("{kind:?}") == "FilesystemLoop"
}

/// A read error that says the folder is no longer there.
fn gone(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// `dir/name` in one allocation (`Path::join` takes two).
fn join(dir: &Path, name: &OsStr) -> PathBuf {
    let mut path = PathBuf::with_capacity(dir.as_os_str().len() + 1 + name.len());
    path.push(dir);
    path.push(name);
    path
}

/// The link `path`, whose target `target` typed, as a frame's [`Link`];
/// `None` when its target is on the way down already (`links`). A
/// `(dev, ino)` hit is confirmed by canonical path, since some file
/// systems hand out inode numbers that are not unique.
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

/// A FIFO, a socket or a device: a reader would block on a FIFO.
fn is_special(t: fs::FileType) -> bool {
    !t.is_dir() && !t.is_file() && !t.is_symlink()
}

/// Can opening `name` reach an entry its folder lists under another
/// name? APFS opens an NFC spelling of a name stored NFD; Win32 drops a
/// trailing `.` or space, opens the device names as devices, and reads
/// `C:` as a drive.
fn opens_as_another(name: &str) -> bool {
    !name.is_ascii() || (cfg!(windows) && win32_rewrites(name.as_bytes()))
}

/// Is `name` in the listing of `dir` as written? A folder that cannot
/// be read leaves it to the `lstat`.
fn listed(dir: &Path, name: &str) -> bool {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .any(|e| e.file_name().as_encoded_bytes() == name.as_bytes()),
        Err(_) => true,
    }
}

/// A trailing `.` or space, a device name (`NUL`, `com1`, `con.txt`), or
/// a `:` (a drive, a stream).
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

/// `p` with `.` and `..` taken out as text (`..` at the root stays at
/// the root), as Node's `path.resolve` does.
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
