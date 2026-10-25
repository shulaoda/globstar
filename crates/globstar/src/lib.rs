//! Glob pattern matching: compile a pattern once, match paths as raw
//! bytes.
//!
//! The dialect is the standard glob family — `*`, `?`, `**`, character
//! classes `[a-z]`/`[!a-z]`, braces `{a,b}`, `\` escapes, and leading
//! `!` negation — specified in the repository's
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
//! arbitrary `&[u8]`, so non-UTF-8 file names compare byte for byte.
//!
//! Only the items re-exported at the crate root are supported API; the
//! `#[doc(hidden)]` modules are internal and may change shape in any
//! release.

#![forbid(unsafe_code)]

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CompileOptions {
    pub dot: bool,
    pub case_insensitive: bool,
    force_nfa: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            dot: true,
            case_insensitive: false,
            force_nfa: false,
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

    #[doc(hidden)]
    pub fn force_nfa(mut self, v: bool) -> Self {
        self.force_nfa = v;
        self
    }
}

#[derive(Clone)]
pub struct Glob {
    engine: Engine,
    negated: bool,
}

#[derive(Debug, Clone)]
enum Engine {
    Literal(LiteralMatcher),
    Segment(Box<SegmentMatcher>),
}

pub trait Matcher {
    fn is_match(&self, path: &[u8]) -> bool;

    fn match_dir(&self, dir_path: &[u8]) -> DirMatch;

    fn static_prefixes(&self) -> Vec<Vec<u8>>;
}

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

impl Glob {
    pub fn new(pattern: &str) -> Result<Self, GlobError> {
        Self::new_with(pattern, CompileOptions::default())
    }

    pub fn new_with(pattern: &str, opts: CompileOptions) -> Result<Self, GlobError> {
        let ast = parse::parse(pattern.as_bytes())?;
        Ok(Self::from_body(ast.body, ast.negated, opts))
    }

    pub fn union<I, S>(patterns: I) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::union_with(patterns, CompileOptions::default())
    }

    pub fn union_with<I, S>(patterns: I, opts: CompileOptions) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let patterns: Vec<S> = patterns.into_iter().collect();
        let mut seen = (patterns.len() > 1).then(|| HashSet::with_capacity(patterns.len()));
        let mut bodies: Vec<Node> = Vec::with_capacity(patterns.len());
        for (index, pattern) in patterns.iter().enumerate() {
            let pattern = pattern.as_ref();
            if seen.as_mut().is_some_and(|seen| !seen.insert(pattern)) {
                continue;
            }
            let parsed = parse::parse(pattern.as_bytes()).map_err(|e| GlobError::InMember {
                index,
                error: Box::new(e),
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
        Ok(Self::from_body(body, false, opts))
    }

    fn from_body(body: Node, negated: bool, opts: CompileOptions) -> Self {
        let engine = match body.to_literal_bytes() {
            Some(lit) => Engine::Literal(LiteralMatcher::new(lit.into(), opts.case_insensitive)),
            None => Engine::Segment(SegmentMatcher::build(body, opts)),
        };
        Self { engine, negated }
    }

    #[inline]
    pub fn is_match(&self, path: &[u8]) -> bool {
        let raw = match &self.engine {
            Engine::Literal(m) => m.is_match(path),
            Engine::Segment(m) => m.is_match(path),
        };
        raw ^ self.negated
    }

    #[doc(hidden)]
    pub fn shape(&self) -> String {
        match &self.engine {
            Engine::Literal(_) => "literal".into(),
            Engine::Segment(m) => m.shape(),
        }
    }

    pub fn static_prefixes(&self) -> Vec<Vec<u8>> {
        if self.negated {
            return vec![Vec::new()];
        }
        match &self.engine {
            Engine::Literal(m) => m.static_prefixes(),
            Engine::Segment(m) => m.static_prefixes(),
        }
    }

    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        if self.negated {
            return DirMatch::new(self.is_match(dir_path), true, false);
        }
        match &self.engine {
            Engine::Literal(m) => m.match_dir(dir_path),
            Engine::Segment(m) => m.match_dir(dir_path),
        }
    }
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
