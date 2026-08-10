use crate::ast::CharClass;
use crate::engine::facts::LiteralFacts;

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
    /// `(?:[^/]+/+)*`, used for leading and middle `**/`. Every absorbed
    /// segment is nonempty, empty segments come from the boundary ops.
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

#[derive(Debug, Clone)]
pub struct OpProgram {
    pub(crate) ops: Vec<Op>,
    pub(crate) facts: LiteralFacts,
    pub(crate) case_insensitive: bool,
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

    pub fn ops(&self) -> &[Op] {
        &self.ops
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
