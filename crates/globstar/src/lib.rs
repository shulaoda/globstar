//! `globstar` — pure glob matcher engine.
//!
//! No filesystem dependencies. Implements the syntax defined in
//! `spec/GLOB_SPEC.md`, with the architecture from `decisions/ADR-001`.
//!
//! ## Engine tiers
//!
//! | Tier | Pattern shape                              | Engine    |
//! |------|--------------------------------------------|-----------|
//! | 0    | Pure literal (`src/main.rs`)               | `Literal` |
//! | 1/2  | Segment-expressible wildcards and braces   | `Segment` |
//! | 1/2  | Segment budget/shape fallback (rare)       | `PikeVm`  |
//!
//! Every tier implements both `is_match` and `match_dir` natively via
//! a precomputed reach-to-accept set — no fallback to recursive
//! backtracking. ReDoS is eliminated by construction (ADR-007).

#![forbid(unsafe_code)]

#[doc(hidden)]
pub mod ast;
#[doc(hidden)]
pub mod dir_match;
#[doc(hidden)]
pub mod engine;
pub mod error;
pub mod factor;
#[doc(hidden)]
pub mod matcher;
#[doc(hidden)]
pub mod options;
#[doc(hidden)]
pub mod parser;

pub use dir_match::DirMatch;
pub use error::GlobError;
pub use matcher::Matcher;
pub use options::CompileOptions;

use ast::{Ast, Node};
use engine::literal::LiteralMatcher;
use engine::ops::lower_owned;
use engine::pikevm::PikeVm;
use engine::segment::SegmentMatcher;
use factor::factor_branches;

/// Tier classification for compiled patterns. Each glob is routed at
/// compile time to exactly one tier. See ADR-001.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Pure literal (no metacharacter). Routed to a byte-compare
    /// matcher; uncommon as a single `Glob::new` argument but
    /// frequent in ignore-list contexts where many literal entries
    /// are merged via `Glob::union`.
    Literal,
    /// Simple wildcards (`*`, `?`, `[]`) without `**` or brace.
    SimpleWildcard,
    /// Contains `**` or brace expansion.
    Globstar,
}

/// A compiled glob pattern.
#[derive(Debug, Clone)]
pub struct Glob {
    /// Tier classification.
    tier: Tier,
    /// Tier-specific matcher engine.
    engine: Engine,
    /// Whether the overall pattern is negated (odd `!` count prefix).
    negated: bool,
}

#[derive(Debug, Clone)]
enum Engine {
    /// Tier 0 — pure literal byte comparison. Inline rather than
    /// boxed: it is the smallest variant, and `LiteralMatcher`'s
    /// `Vec<u8>` already owns its bytes on the heap.
    Literal(LiteralMatcher),
    /// Tier 1/2 — segment-structured matcher for the dominant shapes.
    Segment(Box<SegmentMatcher>),
    /// Linear-time O(n·m) fallback for shapes or bounded expansions
    /// the segment representation cannot express.
    PikeVm(Box<PikeVm>),
}

impl Glob {
    /// Compile a glob pattern with default options.
    pub fn new(pattern: &str) -> Result<Self, GlobError> {
        Self::new_with(pattern, CompileOptions::default())
    }

    /// Compile a glob pattern with custom options.
    pub fn new_with(pattern: &str, opts: CompileOptions) -> Result<Self, GlobError> {
        let ast = parser::parse(pattern.as_bytes())?;
        Self::from_ast(ast, opts)
    }

    /// Compile `patterns` as the boolean OR of their matches, returning a
    /// single `Glob`. Branches are factored and lowered once; separator-
    /// crossing alternatives become bounded segment sequences.
    ///
    /// ## Constraints
    ///
    /// - At least one pattern is required (empty input → [`GlobError::EmptyPatternSet`])
    /// - Negated (`!`-prefixed) patterns are rejected (→ [`GlobError::NegatedInUnion`]).
    ///   For include / exclude semantics, call `Glob::union` twice and
    ///   compose with `inc.is_match(p) && !exc.is_match(p)` at the call
    ///   site
    /// - All patterns share one [`CompileOptions`] — split mixed-options
    ///   groups in the caller
    ///
    /// A single-pattern input is degenerate but still enforces the union
    /// restriction against negated patterns.
    ///
    /// ## Implementation note
    ///
    /// AST-level prefix/suffix factoring lifts shared leading and trailing
    /// fragments out of the branches before lowering, so
    /// `union(["**/*.ts", "**/*.tsx"])` produces the same segment program
    /// as the hand-written `**/*.{ts,tsx}`.
    pub fn union<I, S>(patterns: I) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::union_with(patterns, CompileOptions::default())
    }

    /// [`Glob::union`] with custom options.
    pub fn union_with<I, S>(patterns: I, opts: CompileOptions) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut first: Option<Ast> = None;
        let mut branches: Vec<Node> = Vec::new();
        for (i, pattern) in patterns.into_iter().enumerate() {
            let pattern = pattern.as_ref();
            let parsed = parser::parse(pattern.as_bytes())?;
            if parsed.is_negated() {
                return Err(GlobError::NegatedInUnion {
                    index: i,
                    pattern: pattern.to_string(),
                });
            }
            if first.is_none() && branches.is_empty() {
                first = Some(parsed);
            } else {
                if let Some(ast) = first.take() {
                    branches.push(ast.body);
                }
                branches.push(parsed.body);
            }
        }
        match first {
            Some(ast) => Self::from_ast(ast, opts),
            None if branches.is_empty() => Err(GlobError::EmptyPatternSet),
            None => Self::from_ast(
                Ast {
                    negation_count: 0,
                    body: factor_branches(branches),
                },
                opts,
            ),
        }
    }

    /// Internal: compile from a pre-parsed AST. Used by both `new_with`
    /// (which parses first) and `union_with` (which builds the AST by
    /// merging parsed sub-bodies into a synthetic `Brace`).
    fn from_ast(ast: Ast, opts: CompileOptions) -> Result<Self, GlobError> {
        let negated = ast.is_negated();
        let tier = classify(&ast);
        let engine = match tier {
            Tier::Literal => {
                let lit = ast
                    .body
                    .to_literal_bytes()
                    .expect("Tier::Literal implies pure literal body");
                Engine::Literal(LiteralMatcher::new(lit, opts.case_insensitive))
            }
            Tier::SimpleWildcard | Tier::Globstar => {
                let program = lower_owned(ast.body, opts.case_insensitive);
                match SegmentMatcher::build(program, opts.dot) {
                    Ok(segment) => Engine::Segment(segment),
                    Err(program) => Engine::PikeVm(Box::new(PikeVm::new(program, opts.dot))),
                }
            }
        };
        Ok(Self {
            tier,
            engine,
            negated,
        })
    }

    /// The tier this pattern was routed to.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Diagnostic: which concrete engine variant compiled this
    /// pattern. Intended for bench instrumentation and tests.
    pub fn engine_name(&self) -> &'static str {
        match &self.engine {
            Engine::Literal(_) => "Literal",
            Engine::Segment(_) => "Segment",
            Engine::PikeVm(_) => "PikeVm",
        }
    }

    /// Whether `path` matches this glob.
    #[inline]
    pub fn is_match(&self, path: &[u8]) -> bool {
        let raw = match &self.engine {
            Engine::Literal(m) => m.is_match(path),
            Engine::Segment(m) => m.is_match(path),
            Engine::PikeVm(m) => m.is_match(path),
        };
        if self.negated { !raw } else { raw }
    }

    /// Compute the set of **static path prefixes** for this glob, suitable
    /// for use by a walker to jump directly to the deepest pre-determined
    /// directory instead of scanning the top-level tree.
    ///
    /// The return value is a deduplicated list where each entry is the
    /// longest segment-bounded literal prefix of one brace-expanded program
    /// variant. Shorter prefixes subsume longer ones (so `[src, src/cli]`
    /// deduplicates to `[src]`).
    ///
    /// Examples:
    /// - `src/*.ts` → `[b"src"]`
    /// - `src/**/*.ts` → `[b"src"]`
    /// - `**/*.ts` → `[b""]`
    /// - `{src,tests}/*.rs` → `[b"src", b"tests"]`
    /// - `src/main.rs` → `[b"src/main.rs"]` (fully literal)
    ///
    /// A return value of `[b""]` means "no useful prefix" — the walker
    /// should start from the user-supplied root with no shortcut.
    pub fn static_prefixes(&self) -> Vec<Vec<u8>> {
        match &self.engine {
            Engine::Literal(m) => {
                // Pure literal: the literal IS the prefix. Strip any
                // trailing `/` for walker compatibility.
                let mut bytes = m.literal.clone();
                while bytes.last() == Some(&b'/') {
                    bytes.pop();
                }
                vec![bytes]
            }
            // Cached at build time inside the engine — see
            // `compute_static_prefixes` in `engine::ops`. Already
            // deduplicated; we just clone into the public `Vec<Vec<u8>>`
            // shape that callers expect.
            Engine::Segment(m) => m.static_prefixes().iter().map(|p| p.to_vec()).collect(),
            Engine::PikeVm(m) => m.static_prefixes().iter().map(|p| p.to_vec()).collect(),
        }
    }

    /// `match_dir` query for walker integration. See [`DirMatch`].
    ///
    /// Every engine answers "could some descendant match?" via a
    /// hypothetical `/` step followed by a reach-to-accept lookup
    /// precomputed at build time — no recursive descent.
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        match &self.engine {
            Engine::Literal(m) => m.match_dir(dir_path),
            Engine::Segment(m) => m.match_dir(dir_path),
            Engine::PikeVm(m) => m.match_dir(dir_path),
        }
    }
}

/// Compile-time tier waterfall (ADR-001 §2 "编译期 waterfall").
fn classify(ast: &Ast) -> Tier {
    if ast.body.has_globstar() || contains_brace(&ast.body) {
        // `contains_brace` implies !is_pure_literal (any Brace node
        // breaks pure-literalness), so no extra guard needed here.
        Tier::Globstar
    } else if ast.body.is_pure_literal() {
        Tier::Literal
    } else {
        Tier::SimpleWildcard
    }
}

fn contains_brace(node: &ast::Node) -> bool {
    use ast::Node::*;
    match node {
        Brace(_) => true,
        Concat(xs) => xs.iter().any(contains_brace),
        _ => false,
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
