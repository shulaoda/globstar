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
//! segment-structured) — [`Glob::engine_name`] reveals the choice for
//! diagnostics.
//!
//! Only the items re-exported at the crate root are supported API; the
//! `#[doc(hidden)]` modules are internal and may change shape in any
//! release.

#![forbid(unsafe_code)]

#[doc(hidden)]
pub mod ast;
mod dir_match;
mod distribute;
#[doc(hidden)]
pub mod engine;
pub mod error;
mod factor;
#[doc(hidden)]
pub mod parser;
mod resolve;

pub use dir_match::DirMatch;
pub use error::GlobError;

use std::collections::HashSet;

use ast::{Ast, Node};
use engine::literal::LiteralMatcher;
use engine::ops::lower;
use engine::segment::SegmentMatcher;
use factor::factor_branches;

/// Syntactic complexity class of a compiled pattern — useful for
/// diagnostics, not consulted at match time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Pure literal (no metacharacter).
    Literal,
    /// Simple wildcards (`*`, `?`, `[]`) without `**` or brace.
    SimpleWildcard,
    /// Contains `**` or brace expansion.
    Globstar,
}

/// Compile-time options for [`Glob::new_with`] and [`Glob::union_with`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CompileOptions {
    /// When `false`, wildcards (`*` `?` `[^x]`) cannot consume a leading
    /// `.` — dotfile protection. `Glob` defaults to `true`, `Walk` to
    /// `false` (GLOB_SPEC.md §11.1 / §12.4).
    pub dot: bool,

    /// ASCII-only case folding, `false` by default; non-ASCII bytes
    /// compare verbatim (GLOB_SPEC.md §11.2 / §12.5).
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
    pub fn dot(mut self, v: bool) -> Self {
        self.dot = v;
        self
    }

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
#[derive(Debug, Clone)]
pub struct Glob {
    tier: Tier,
    engine: Engine,
    negated: bool,
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
        let ast = parser::parse(pattern.as_bytes())?;
        Self::from_ast(ast, opts)
    }

    /// Compile the boolean-OR union of several patterns into one `Glob`
    /// with default options.
    ///
    /// `!`-negated members are rejected with
    /// [`GlobError::NegatedInUnion`]; an empty iterator with
    /// [`GlobError::EmptyPatternSet`]. A pattern given twice counts
    /// once. The members share one expansion budget
    /// ([`GlobError::BraceExpansionTooLarge`]).
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
        let mut seen = HashSet::with_capacity(patterns.len());
        let mut bodies: Vec<Node> = Vec::with_capacity(patterns.len());
        let mut budget = error::MAX_EXPANSION;
        for (index, pattern) in patterns.iter().enumerate() {
            let pattern = pattern.as_ref();
            if !seen.insert(pattern) {
                continue;
            }
            let parsed = parser::parse_within(pattern.as_bytes(), &mut budget)?;
            if parsed.is_negated() {
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
        Self::from_ast(
            Ast {
                negation_count: 0,
                body,
            },
            opts,
        )
    }

    fn from_ast(ast: Ast, opts: CompileOptions) -> Result<Self, GlobError> {
        let negated = ast.is_negated();
        let (tier, engine) = match ast.body.to_literal_bytes() {
            Some(lit) => (
                Tier::Literal,
                Engine::Literal(LiteralMatcher::new(lit, opts.case_insensitive)),
            ),
            None => {
                let tier = if ast.body.has_globstar() || contains_brace(&ast.body) {
                    Tier::Globstar
                } else {
                    Tier::SimpleWildcard
                };
                let program = lower(ast.body, opts.case_insensitive);
                let engine = Engine::Segment(SegmentMatcher::build(program, opts.dot)?);
                (tier, engine)
            }
        };
        Ok(Self {
            tier,
            engine,
            negated,
        })
    }

    /// The pattern's syntactic [`Tier`].
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Which engine compilation chose: `"Literal"` or `"Segment"`.
    /// Diagnostic only.
    pub fn engine_name(&self) -> &'static str {
        match &self.engine {
            Engine::Literal(_) => "Literal",
            Engine::Segment(_) => "Segment",
        }
    }

    #[inline]
    /// Whole-path match against raw path bytes.
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

    /// Literal path prefixes a walker can seed traversal from: every
    /// matching path starts with one of the returned byte strings.
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
    /// A negated pattern is always [`DirMatch::Descend`]: what it
    /// excludes below a directory cannot be told from the directory.
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        if self.negated {
            return DirMatch::Descend;
        }
        match &self.engine {
            Engine::Literal(m) => m.match_dir(dir_path),
            Engine::Segment(m) => m.match_dir(dir_path),
        }
    }
}

fn contains_brace(node: &Node) -> bool {
    match node {
        Node::Brace(_) => true,
        Node::Concat(xs) => xs.iter().any(contains_brace),
        _ => false,
    }
}

/// The matcher surface, implemented by [`Glob`]. Object-safe, so a
/// consumer can hold `Box<dyn Matcher>` when it mixes matcher sources.
pub trait Matcher {
    /// Whole-path match: is `path` in the pattern's language?
    fn is_match(&self, path: &[u8]) -> bool;

    /// Directory-level verdict for pruning (see [`DirMatch`]).
    fn match_dir(&self, dir_path: &[u8]) -> DirMatch;

    /// Literal path prefixes a walker can seed traversal from. Every
    /// matching path starts with one of the returned byte strings.
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
