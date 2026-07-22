//! Linear instructions consumed by every non-literal matcher backend.

use crate::ast::CharClass;
use crate::engine::facts::LiteralFacts;

/// One executable instruction in a compiled glob program.
#[derive(Debug, Clone)]
pub enum Op {
    /// Match a literal byte sequence verbatim.
    Lit(Vec<u8>),
    /// Match a single non-separator byte.
    AnyChar,
    /// Match zero or more non-separator bytes.
    Star,
    /// Match one byte against the class.
    Class(CharClass),
    /// Match exactly one path separator.
    Sep,
    /// Match one or more path separators at a lenient globstar boundary.
    SepRun,
    /// Raw `**`; internal to lowering and normalized before publication.
    Globstar,
    /// `(?:[^/]*/)*`, used for leading and middle `**/`.
    OptSegmentsSlash,
    /// `/.*`, used for strict trailing `/**`.
    SlashAnything,
    /// `.*`, used for a bare `**`.
    GlobstarAny,
    /// Zero or more leading platform separators for pattern-head `**/`.
    LeadingSeps,
    /// Brace alternation. Branches remain nested rather than cartesian-expanded.
    Alternation(Vec<Vec<Op>>),
}

/// Normalized executable program plus compile-time literal facts.
#[derive(Debug, Clone)]
pub struct OpProgram {
    ops: Vec<Op>,
    facts: LiteralFacts,
    case_insensitive: bool,
}

impl OpProgram {
    pub(super) fn from_normalized(ops: Vec<Op>, case_insensitive: bool) -> Self {
        debug_assert!(is_normalized(&ops));
        let facts = LiteralFacts::extract(&ops, case_insensitive);
        Self {
            ops,
            facts,
            case_insensitive,
        }
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    pub fn facts(&self) -> &LiteralFacts {
        &self.facts
    }

    pub fn case_insensitive(&self) -> bool {
        self.case_insensitive
    }

    /// Consume the immutable normalized program after a backend has derived
    /// any structural metadata it needs.
    pub fn into_parts(self) -> (Vec<Op>, LiteralFacts, bool) {
        (self.ops, self.facts, self.case_insensitive)
    }
}

fn is_normalized(ops: &[Op]) -> bool {
    let mut previous_lit = false;
    let mut previous_star = false;
    for op in ops {
        match op {
            Op::Globstar => return false,
            Op::Lit(_) if previous_lit => return false,
            Op::Star if previous_star => return false,
            Op::Alternation(branches) if !branches.iter().all(|b| is_normalized(b)) => {
                return false;
            }
            _ => {}
        }
        previous_lit = matches!(op, Op::Lit(_));
        previous_star = matches!(op, Op::Star);
    }
    true
}
