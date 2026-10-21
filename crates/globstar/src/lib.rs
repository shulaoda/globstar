//! Glob pattern matching: compile a pattern once, match paths as raw
//! bytes.
//!
//! The dialect is the standard glob family — `*`, `?`, `**`, character
//! classes `[a-z]`/`[!a-z]`, braces `{a,b}`, `\` escapes, and leading
//! `!` whole-pattern negation — specified precisely in the repository's
//! `references/spec/GLOB_SPEC.md`. `@globstar/core` (npm) is the
//! behaviorally identical JS twin. The filesystem walker built on this
//! crate lives in `globstar-walk`.
//!
//! # Example
//!
//! ```
//! use globstar::{DirMatch, Glob};
//!
//! let glob = Glob::new("src/**/*.rs")?;
//! assert!(glob.is_match(b"src/engine/mod.rs"));
//! assert!(!glob.is_match(b"README.md"));
//!
//! // A walker selecting files walks into a directory while something
//! // below it may match:
//! assert_eq!(glob.match_dir(b"src/engine"), DirMatch::Descend);
//! assert!(glob.match_dir(b"src/engine").may_match_below());
//! assert_eq!(glob.match_dir(b"target"), DirMatch::Pruned);
//!
//! // A walker excluding files skips a directory whole only when
//! // everything below it matches:
//! let exclude = Glob::union(["**/node_modules/**", "**/*.log"])?;
//! assert!(exclude.match_dir(b"/p/node_modules").matches_all_below());
//! assert!(!exclude.match_dir(b"/p/foo.log").matches_all_below());
//! # Ok::<(), globstar::GlobError>(())
//! ```
//!
//! Patterns are `&str` (their UTF-8 bytes are what match); paths are
//! arbitrary `&[u8]`, so non-UTF-8 filenames compare byte-for-byte.
//! Compilation picks one of two engines automatically (pure-literal or
//! segment-structured).
//!
//! Only the items re-exported at the crate root are supported API; the
//! `#[doc(hidden)]` modules are internal and may change shape in any
//! release.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod dir_match;
mod engine;
pub mod error;
#[doc(hidden)]
pub mod parse;
mod union;

pub use dir_match::DirMatch;
pub use error::GlobError;

use std::collections::HashSet;

use engine::literal::LiteralMatcher;
use engine::segment::SegmentMatcher;
use parse::ast::Node;
use union::factor_branches;

/// Compile-time options for [`Glob::new_with`] and [`Glob::union_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CompileOptions {
    /// When `false`, wildcards (`*` `?` `[^x]`) cannot consume a leading
    /// `.` — dotfile protection. `Glob` defaults to `true`, `Walk` to
    /// `false` (GLOB_SPEC.md §3.1 / §12).
    pub dot: bool,

    /// ASCII-only case folding, `false` by default; non-ASCII bytes
    /// compare verbatim (GLOB_SPEC.md §13 / §14.3).
    pub case_insensitive: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            dot: true,
            case_insensitive: false,
        }
    }
}

impl CompileOptions {
    /// These options with [`dot`](Self::dot) set to `v`.
    pub fn dot(mut self, v: bool) -> Self {
        self.dot = v;
        self
    }

    /// These options with [`case_insensitive`](Self::case_insensitive)
    /// set to `v`.
    pub fn case_insensitive(mut self, v: bool) -> Self {
        self.case_insensitive = v;
        self
    }
}

/// A compiled glob pattern (or [`union`](Glob::union) of patterns).
///
/// Cheap to match against many paths; compile once and reuse. `Glob`
/// is `Send + Sync + Clone`, so it can be shared across threads
/// freely. See the [crate docs](crate) for an overview and the
/// repository's `GLOB_SPEC.md` for the dialect.
#[derive(Clone)]
pub struct Glob {
    engine: Engine,
    negated: bool,
}

/// The engine and the negation; the compiled tables are left out (a
/// union's run to kilobytes).
impl std::fmt::Debug for Glob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let engine = match &self.engine {
            Engine::Literal(_) => "Literal",
            Engine::Segment(_) => "Segment",
        };
        f.debug_struct("Glob")
            .field("engine", &engine)
            .field("negated", &self.negated)
            .finish()
    }
}

#[derive(Debug, Clone)]
enum Engine {
    /// Pure literal byte comparison.
    Literal(LiteralMatcher),
    /// The segment-structured matcher for everything else.
    Segment(Box<SegmentMatcher>),
}

impl Glob {
    /// Compile a pattern with default options (`dot: true`,
    /// `case_insensitive: false`).
    pub fn new(pattern: &str) -> Result<Self, GlobError> {
        Self::new_with(pattern, CompileOptions::default())
    }

    /// Compile a pattern with explicit [`CompileOptions`].
    pub fn new_with(pattern: &str, opts: CompileOptions) -> Result<Self, GlobError> {
        let ast = parse::parse(pattern.as_bytes())?;
        Self::from_body(ast.body, ast.negated, opts)
    }

    /// Compile the boolean-OR union of several patterns into one `Glob`
    /// with default options.
    ///
    /// A negated member (an odd number of leading `!`) is rejected with
    /// [`GlobError::NegatedInUnion`]; `!!x` is just `x`. An empty iterator
    /// fails with [`GlobError::EmptyPatternSet`], and a member that does
    /// not compile with [`GlobError::InMember`], which names it by index
    /// ([`GlobError::root`] gives the error inside). A pattern given
    /// twice counts once. The members share one expansion budget
    /// ([`GlobError::BraceExpansionTooLarge`]) and, compiled as one,
    /// one bound on compile work: a [`GlobError::TooComplex`] is the
    /// list's, not wrapped in `InMember`, and members that each compile
    /// alone can fail together.
    pub fn union<I, S>(patterns: I) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::union_with(patterns, CompileOptions::default())
    }

    /// [`union`](Self::union) with explicit [`CompileOptions`].
    pub fn union_with<I, S>(patterns: I, opts: CompileOptions) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // The same pattern twice adds nothing to the union: it is parsed
        // once and counts once against the expansion budget. Members that
        // only differ in spelling fold into one when factored.
        let patterns: Vec<S> = patterns.into_iter().collect();
        let mut seen = (patterns.len() > 1).then(|| HashSet::with_capacity(patterns.len()));
        let mut bodies: Vec<Node> = Vec::with_capacity(patterns.len());
        let mut budget = error::MAX_EXPANSION;
        for (index, pattern) in patterns.iter().enumerate() {
            let pattern = pattern.as_ref();
            if seen.as_mut().is_some_and(|seen| !seen.insert(pattern)) {
                continue;
            }
            let parsed = parse::parse_within(pattern.as_bytes(), &mut budget).map_err(|e| {
                GlobError::InMember {
                    index,
                    error: Box::new(e),
                }
            })?;
            if parsed.negated {
                return Err(GlobError::NegatedInUnion {
                    index,
                    pattern: pattern.to_string(),
                });
            }
            bodies.push(parsed.body);
        }
        let body = match bodies.len() {
            0 => return Err(GlobError::EmptyPatternSet),
            1 => bodies.pop().unwrap(),
            _ => factor_branches(bodies),
        };
        Self::from_body(body, false, opts)
    }

    fn from_body(body: Node, negated: bool, opts: CompileOptions) -> Result<Self, GlobError> {
        let engine = match body.to_literal_bytes() {
            Some(lit) => Engine::Literal(LiteralMatcher::new(lit.into(), opts.case_insensitive)),
            None => Engine::Segment(SegmentMatcher::build(
                body,
                opts.dot,
                opts.case_insensitive,
            )?),
        };
        Ok(Self { engine, negated })
    }

    /// Whole-path match against raw path bytes. The path is matched as
    /// given: with `dot: true` the names `.` and `..` are plain bytes,
    /// so `src/*/a.ts` matches `src/../a.ts`. Normalize paths from
    /// outside first (GLOB_SPEC §3.1).
    #[inline]
    pub fn is_match(&self, path: &[u8]) -> bool {
        let raw = match &self.engine {
            Engine::Literal(m) => m.is_match(path),
            Engine::Segment(m) => m.is_match(path),
        };
        raw ^ self.negated
    }

    /// [`is_match`](Self::is_match) with the segment engine's anchored
    /// chains bypassed: the NFA over the whole pattern graph decides.
    /// A test hook that cross-checks the two code paths.
    #[doc(hidden)]
    pub fn is_match_nfa(&self, path: &[u8]) -> bool {
        let raw = match &self.engine {
            Engine::Literal(m) => m.is_match(path),
            Engine::Segment(m) => m.is_match_nfa(path),
        };
        raw ^ self.negated
    }

    /// How compilation laid the pattern out: its elements in order, its
    /// chains and the form of its graph NFA, or `literal`. A test hook
    /// that pins the engine's routing (`fixtures/engine-shapes.tsv`),
    /// where a slower path would still give the right answers.
    #[doc(hidden)]
    pub fn shape(&self) -> String {
        match &self.engine {
            Engine::Literal(_) => "literal".into(),
            Engine::Segment(m) => m.shape(),
        }
    }

    /// Literal path prefixes a walker can seed traversal from: for each
    /// expansion of the pattern's braces, its literal head up to its last
    /// whole segment, with heads a shorter one covers removed
    /// (`{src,lib}/index.ts` gives `src/index.ts` and `lib/index.ts`). The
    /// heads are bytes spelled with `/` and in the pattern's case: every
    /// match starts with one of them byte for byte, except that under
    /// `case_insensitive` a match may differ in case, and on Windows in
    /// its separators (`\`). An empty prefix means the root.
    pub fn static_prefixes(&self) -> Vec<Vec<u8>> {
        if self.negated {
            return vec![Vec::new()];
        }
        match &self.engine {
            Engine::Literal(m) => m.static_prefixes(),
            Engine::Segment(m) => m.static_prefixes(),
        }
    }

    /// What the pattern says about a directory (see [`DirMatch`]).
    ///
    /// A negated pattern says whether the directory itself matches, and
    /// that something below may: [`DirMatch::DescendAndMatch`] or
    /// [`DirMatch::Descend`], never a covering verdict. Ask about a
    /// directory without a trailing separator (`a`, not `a/`; GLOB_SPEC
    /// §15.3).
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        // Anything below may match a negated pattern; whether `dir_path`
        // itself does is exact (§15.4).
        if self.negated {
            return DirMatch::new(self.is_match(dir_path), true, false);
        }
        match &self.engine {
            Engine::Literal(m) => m.match_dir(dir_path),
            Engine::Segment(m) => m.match_dir(dir_path),
        }
    }
}

/// The matcher surface, implemented by [`Glob`]. Object-safe, so a
/// consumer can hold `Box<dyn Matcher>` when it mixes matcher sources.
pub trait Matcher {
    /// Whole-path match: is `path` in the pattern's language?
    fn is_match(&self, path: &[u8]) -> bool;

    /// Directory-level verdict for pruning (see [`DirMatch`]).
    fn match_dir(&self, dir_path: &[u8]) -> DirMatch;

    /// Literal path prefixes a walker can seed traversal from (see
    /// [`Glob::static_prefixes`]).
    fn static_prefixes(&self) -> Vec<Vec<u8>>;
}

impl Matcher for Glob {
    fn is_match(&self, path: &[u8]) -> bool {
        Glob::is_match(self, path)
    }

    fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        Glob::match_dir(self, dir_path)
    }

    fn static_prefixes(&self) -> Vec<Vec<u8>> {
        Glob::static_prefixes(self)
    }
}
