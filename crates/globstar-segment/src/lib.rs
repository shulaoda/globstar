//! `globstar-segment` — the segment-structured matcher (SSM) as a
//! standalone crate, benchmarked head-to-head against the `globstar`
//! crate's automata engines and third-party libraries.
//!
//! Shares the `globstar` crate's parser, lowering, options, error
//! type, and `DirMatch`/`Matcher` contracts — both crates compile the
//! exact same dialect (GLOB_SPEC v0.2.0) and are held to the same
//! corpus and cross-runtime differential fuzzer. Only the matching
//! engine differs:
//!
//! | Tier | Pattern shape                       | Engine                 |
//! |------|-------------------------------------|------------------------|
//! | 0    | Pure literal                        | `LiteralMatcher` (shared) |
//! | 1    | Segment-expressible (≈ all real patterns) | SSM (`engine` module) |
//! | 2    | Glued globstars, budget overflows (rare) | `PikeVm` (shared)  |
//!
//! The fallback is deliberately just the Pike VM: it is the only
//! *total* engine (any pattern, guaranteed O(n·m) — the ReDoS
//! soundness floor), it compiles in ~1µs, and the patterns that reach
//! it are degenerate stress shapes where match micro-speed is
//! irrelevant. Keeping the eager DFA here would re-import the exact
//! subset-construction cost the SSM exists to eliminate, for a tier
//! that still could not drop the Pike VM (the DFA has its own state
//! cap and falls back to it).
//!
//! Design + errata: `references/decisions/segment-engine-design.md`;
//! theory note `references/theory/07-segment-matcher.md`.

#![forbid(unsafe_code)]

mod engine;

pub use globstar::{CompileOptions, DirMatch, GlobError, Matcher, Tier};

use engine::SegmentMatcher;
use globstar::ast::{Ast, Node};
use globstar::engine::literal::LiteralMatcher;
use globstar::engine::ops::lower_owned;
use globstar::engine::pikevm::PikeVm;
use globstar::factor::factor_branches;
use globstar::parser;

/// A compiled glob pattern, SSM-first. Public surface mirrors
/// [`globstar::Glob`].
#[derive(Debug, Clone)]
pub struct SegGlob {
    tier: Tier,
    engine: Engine,
    negated: bool,
}

#[derive(Debug, Clone)]
enum Engine {
    Literal(LiteralMatcher),
    Segment(Box<SegmentMatcher>),
    PikeVm(Box<PikeVm>),
}

impl SegGlob {
    /// Compile a glob pattern with default options.
    pub fn new(pattern: &str) -> Result<Self, GlobError> {
        Self::new_with(pattern, CompileOptions::default())
    }

    /// Compile a glob pattern with custom options.
    pub fn new_with(pattern: &str, opts: CompileOptions) -> Result<Self, GlobError> {
        let ast = parser::parse(pattern.as_bytes())?;
        Self::from_ast(ast, opts)
    }

    /// Compile `patterns` as the boolean OR of their matches. The SSM
    /// compiles the merged brace as fork sequences in one linear pass
    /// — no NFA probe, no per-pattern decomposition.
    pub fn union<I, S>(patterns: I) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::union_with(patterns, CompileOptions::default())
    }

    /// [`SegGlob::union`] with custom options.
    pub fn union_with<I, S>(patterns: I, opts: CompileOptions) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // `first` holds a lone pattern's full Ast (degenerate one-
        // pattern union = plain compile); with a second pattern both
        // move into `branches` for the merged brace.
        let mut first: Option<Ast> = None;
        let mut branches: Vec<Node> = Vec::new();
        for (i, p) in patterns.into_iter().enumerate() {
            let p = p.as_ref();
            let parsed = parser::parse(p.as_bytes())?;
            if parsed.is_negated() {
                return Err(GlobError::NegatedInUnion {
                    index: i,
                    pattern: p.to_string(),
                });
            }
            if first.is_none() && branches.is_empty() {
                first = Some(parsed);
            } else {
                if let Some(f) = first.take() {
                    branches.push(f.body);
                }
                branches.push(parsed.body);
            }
        }
        match first {
            Some(f) => Self::from_ast(f, opts),
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
                    Ok(seg) => Engine::Segment(seg),
                    // Not segment-expressible (glued globstars via
                    // braces, escaped separators, budget overflows —
                    // ~0.5% of the corpus, all degenerate shapes):
                    // the total, ReDoS-bounded Pike VM.
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

    /// Diagnostic: which concrete engine compiled this pattern.
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

    /// `match_dir` query for walker integration. See [`DirMatch`].
    pub fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        match &self.engine {
            Engine::Literal(m) => m.match_dir(dir_path),
            Engine::Segment(m) => m.match_dir(dir_path),
            Engine::PikeVm(m) => m.match_dir(dir_path),
        }
    }

    /// Static path prefixes for walker traversal seeding — same
    /// contract as [`globstar::Glob::static_prefixes`].
    pub fn static_prefixes(&self) -> Vec<Vec<u8>> {
        match &self.engine {
            Engine::Literal(m) => {
                let mut bytes = m.literal_bytes().to_vec();
                while bytes.last() == Some(&b'/') {
                    bytes.pop();
                }
                vec![bytes]
            }
            Engine::Segment(m) => m.static_prefixes().iter().map(|p| p.to_vec()).collect(),
            Engine::PikeVm(m) => m.static_prefixes().iter().map(|p| p.to_vec()).collect(),
        }
    }
}

/// Same compile-time tier waterfall as `globstar::Glob`.
fn classify(ast: &Ast) -> Tier {
    if ast.body.has_globstar() || contains_brace(&ast.body) {
        Tier::Globstar
    } else if ast.body.is_pure_literal() {
        Tier::Literal
    } else {
        Tier::SimpleWildcard
    }
}

fn contains_brace(node: &Node) -> bool {
    match node {
        Node::Brace(_) => true,
        Node::Concat(xs) => xs.iter().any(contains_brace),
        _ => false,
    }
}

impl Matcher for SegGlob {
    fn is_match(&self, path: &[u8]) -> bool {
        SegGlob::is_match(self, path)
    }

    fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        SegGlob::match_dir(self, dir_path)
    }

    fn static_prefixes(&self) -> Vec<Vec<u8>> {
        SegGlob::static_prefixes(self)
    }
}
