//! `globstar` — pure glob matcher engine.
//!
//! No filesystem dependencies. Implements the syntax defined in
//! `spec/GLOB_SPEC.md`, with the architecture from `decisions/ADR-001`.
//!
//! ## Engine tiers
//!
//! | Tier | Pattern shape                              | Engine         | Speed        |
//! |------|--------------------------------------------|----------------|--------------|
//! | 0    | Pure literal (`src/main.rs`)               | `Literal`      | ~0.5 ns/byte |
//! | 1/2  | `*`, `?`, `[...]`, `**`, `{a,b}`           | `ThompsonDfa`  | ~1-2 ns/byte |
//! | 1/2  | DFA state cap exceeded (very rare)         | `PikeVm`       | ~10 ns/byte  |
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
use engine::ops::lower;
use engine::pikevm::PikeVm;
use engine::thompson::Thompson;
use engine::thompson_dfa::ThompsonDfa;
use factor::factor_branches;

/// NFA state count above which `Glob::union` decomposes into per-pattern
/// engines (`Engine::Or`) instead of building one merged DFA. Matches
/// the DFA's `StateKey` fast-path budget — beyond 64 states the wide-
/// path subset construction's compile cost balloons (huge-set-pos: 414
/// µs at 223 NFA states vs ~60 µs decomposed). 95% of realistic
/// patterns fit under this threshold (per `tools/memory-check/src/nfa_survey.rs`).
const NFA_FAST_PATH_LIMIT: usize = 64;

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
    /// Tier 1/2 — eager DFA subset-constructed from a Thompson NFA.
    /// Primary engine for every non-literal pattern (~1-2 ns/byte hot
    /// loop). Falls back to [`Engine::PikeVm`] when subset
    /// construction overruns
    /// [`engine::thompson_dfa::MAX_DFA_STATES`].
    ThompsonDfa(Box<ThompsonDfa>),
    /// Linear-time O(n·m) NFA simulation. Used when the DFA's state
    /// cap is exceeded — rare in practice but guarantees no ReDoS
    /// surface even on adversarial brace-heavy patterns.
    PikeVm(Box<PikeVm>),
    /// Per-pattern decomposition for `Glob::union` when the merged
    /// NFA would exceed [`NFA_FAST_PATH_LIMIT`]. Each child compiles
    /// independently on the DFA fast-path; `is_match` ORs results,
    /// `match_dir` aggregates `(exact, prefix)` flags. Avoids the
    /// wide-path subset construction's compile blowup (huge-set-pos:
    /// 414 µs → ~60 µs) at the cost of N×match-time-overhead vs a
    /// single merged DFA (huge-set-pos: 132 ns → ~321 ns).
    Or(Box<[Glob]>),
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
    /// single `Glob`. Lowers to one DFA via `N_BRACE` alternation, which
    /// in turn shares states for any common prefix/suffix across
    /// branches — measurably faster to compile and smaller in memory
    /// than aggregating N independent matchers.
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
    /// A single-pattern input is degenerate and forwards to
    /// [`Glob::new_with`].
    ///
    /// ## Implementation note
    ///
    /// AST-level prefix/suffix factoring lifts shared leading and trailing
    /// fragments out of the brace branches before lowering, so
    /// `union(["**/*.ts", "**/*.tsx"])` produces the same DFA as the
    /// hand-written `**/*.{ts,tsx}` rather than two duplicated `**/*.`
    /// prefixes.
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
        let raw: Vec<String> = patterns
            .into_iter()
            .map(|s| s.as_ref().to_string())
            .collect();
        if raw.is_empty() {
            return Err(GlobError::EmptyPatternSet);
        }
        if raw.len() == 1 {
            return Self::new_with(&raw[0], opts);
        }

        let mut branches: Vec<Node> = Vec::with_capacity(raw.len());
        for (i, p) in raw.iter().enumerate() {
            let parsed = parser::parse(p.as_bytes())?;
            if parsed.is_negated() {
                return Err(GlobError::NegatedInUnion {
                    index: i,
                    pattern: p.clone(),
                });
            }
            branches.push(parsed.body);
        }

        // Probe merged NFA size before committing to one strategy.
        // Thompson::compile is fast (~1-15 µs even at 223 states) —
        // negligible vs either path's actual work.
        let merged_body = factor_branches(branches);
        let probe_program = lower(&merged_body, opts.case_insensitive);
        let probe_thompson = Thompson::compile(&probe_program, opts.dot);

        if probe_thompson.state_count() <= NFA_FAST_PATH_LIMIT {
            // Fast-path: merged NFA fits the DFA's `u64`-keyed dedup
            // budget, so subset construction stays cheap. One merged
            // state machine, 1 table-lookup per byte at match.
            let merged_ast = Ast {
                negation_count: 0,
                body: merged_body,
            };
            return Self::from_ast(merged_ast, opts);
        }

        // Wide-path detected — decompose into per-pattern Globs.
        // Each child's NFA is small (single pattern), so each compiles
        // on the DFA fast-path. Skips the merged wide-path subset
        // construction's pathological compile cost.
        let children: Result<Vec<Glob>, GlobError> =
            raw.iter().map(|p| Self::new_with(p, opts)).collect();
        Ok(Self {
            tier: Tier::Globstar,
            engine: Engine::Or(children?.into_boxed_slice()),
            negated: false,
        })
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
                let program = lower(&ast.body, opts.case_insensitive);
                match ThompsonDfa::build(program, opts.dot) {
                    Ok(dfa) => Engine::ThompsonDfa(dfa),
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

    /// Diagnostic: `(num_states, num_byte_classes)` of the compiled
    /// DFA if the engine has one, else `None`. Intended for bench
    /// and debugging use only.
    pub fn dfa_size(&self) -> Option<(usize, usize)> {
        match &self.engine {
            Engine::ThompsonDfa(m) => Some((m.num_states(), m.num_byte_classes())),
            _ => None,
        }
    }

    /// Diagnostic: which concrete engine variant compiled this
    /// pattern. Intended for bench instrumentation and tests.
    pub fn engine_name(&self) -> &'static str {
        match &self.engine {
            Engine::Literal(_) => "Literal",
            Engine::ThompsonDfa(_) => "ThompsonDfa",
            Engine::PikeVm(_) => "PikeVm",
            Engine::Or(_) => "Or",
        }
    }

    /// Whether `path` matches this glob.
    #[inline]
    pub fn is_match(&self, path: &[u8]) -> bool {
        let raw = match &self.engine {
            Engine::Literal(m) => m.is_match(path),
            Engine::ThompsonDfa(m) => m.is_match(path),
            Engine::PikeVm(m) => m.is_match(path),
            Engine::Or(children) => children.iter().any(|g| g.is_match(path)),
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
            Engine::ThompsonDfa(m) => m.static_prefixes().iter().map(|p| p.to_vec()).collect(),
            Engine::PikeVm(m) => m.static_prefixes().iter().map(|p| p.to_vec()).collect(),
            // Per-pattern Or: union of children's prefixes, deduped.
            // Walker uses this to seed traversal; each child's prefix
            // names a possible root. Dedupe collapses overlap (e.g.
            // `[src, src/cli]` → `[src]`).
            Engine::Or(children) => {
                let raw: Vec<Vec<u8>> = children.iter().flat_map(|g| g.static_prefixes()).collect();
                engine::ops::dedupe_prefixes(raw)
            }
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
            Engine::ThompsonDfa(m) => m.match_dir(dir_path),
            Engine::PikeVm(m) => m.match_dir(dir_path),
            // Aggregate children's `(exact, prefix)` flags. Any child
            // descend-and-matches → return DescendAndMatch (strongest);
            // else combine via `from_exact_prefix`.
            Engine::Or(children) => {
                let mut exact = false;
                let mut prefix = false;
                for child in children.iter() {
                    match child.match_dir(dir_path) {
                        DirMatch::DescendAndMatch => return DirMatch::DescendAndMatch,
                        DirMatch::Match => exact = true,
                        DirMatch::Descend => prefix = true,
                        DirMatch::Pruned => {}
                    }
                    if exact && prefix {
                        return DirMatch::DescendAndMatch;
                    }
                }
                DirMatch::from_exact_prefix(exact, prefix)
            }
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
