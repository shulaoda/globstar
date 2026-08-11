mod compile;
mod exec;
mod seg_nfa;

use crate::dir_match::DirMatch;
use crate::engine::facts::LiteralFacts;
use crate::engine::ops::{OpProgram, compute_static_prefixes};

use seg_nfa::SegNfa;

const MAX_FORKS: usize = 64;
const MAX_SEQ_STATES: usize = 64;

#[derive(Debug, Clone)]
enum Elem {
    /// Literal segment (possibly empty; never contains a separator).
    Lit(Box<[u8]>),
    /// In-segment wildcard matcher.
    Wild(Wild),
    /// Globstar absorbing ≥ 0 segments, empty ones included.
    G0,
    /// `G0` whose absorbed run may not begin with an empty segment
    /// (a spliced `**/` behind a strict `Sep`, e.g. `a/{**/b,c}`).
    G0Strict,
    /// Globstar absorbing ≥ 1 segment (trailing `/**`: `a/**`
    /// matches `a/` but not `a`).
    G1,
}

impl Elem {
    #[inline]
    fn is_globstar(&self) -> bool {
        matches!(self, Elem::G0 | Elem::G0Strict | Elem::G1)
    }
}

#[derive(Debug, Clone)]
struct Wild {
    kind: WildKind,
    /// Minimum segment byte length (literal parts + `?` count; for
    /// `AffixSet` excludes the per-branch suffix, unused by
    /// `Generic`).
    min_len: u32,
    /// `false` ⇒ length must equal `min_len` exactly (no `*`).
    variable: bool,
    /// Reject dot-led segments (wildcard-led matcher on a
    /// `dot=false` compile).
    dot_protect: bool,
}

#[derive(Debug, Clone)]
enum WildKind {
    /// `lit (*|?)+ lit` and degenerate forms (`*`, `*lit`, `lit*`,
    /// `a?b`): compare the affixes, the length rules do the rest.
    Affix {
        prefix: Box<[u8]>,
        suffix: Box<[u8]>,
    },
    /// `lit (*|?)+ {lit,…}` (`*.{ts,tsx}`): the prefix plus one
    /// suffix out of a set.
    AffixSet {
        prefix: Box<[u8]>,
        suffixes: Box<[Box<[u8]>]>,
    },
    /// Everything else: a mini Thompson NFA over the in-segment ops,
    /// simulated with a `u64` active set.
    Generic(Box<SegNfa>),
}

/// One compiled fork sequence plus its precomputed NFA metadata.
#[derive(Debug, Clone)]
struct ElemSeq {
    elems: Box<[Elem]>,
    /// Index of the single globstar element (fast anchored path);
    /// `u8::MAX` when the count differs from one.
    single_g: u8,
    /// Number of globstar elements.
    g_count: u8,
    /// All-literal head of a single-globstar sequence, pre-joined
    /// with `/` (`"src/"`): one sep-aware compare instead of the
    /// segment loop. Empty ⇒ not applicable.
    joined_head: Box<[u8]>,
    /// Per-element entry state id (ascending). `G0Strict`/`G1` own a
    /// second body state; accept is `num_states - 1`.
    state_of: Box<[u8]>,
    /// Inverse of `state_of`: owning element per state (accept slot
    /// unused).
    elem_of: Box<[u8]>,
    num_states: u8,
    /// `eps[s]`: states reachable from `s` consuming zero segments
    /// (globstar skips), including `s`.
    eps: Box<[u64]>,
    /// States that can still consume ≥ 1 segment on a path to
    /// accept; drives `match_dir`'s prefix bit.
    reach1: u64,
    /// Byte suffix every match of this fork must end with (empty =
    /// none). Consulted only by multi-fork matchers.
    quick_suffix: Box<[u8]>,
}

#[derive(Debug, Clone)]
pub(crate) struct SegmentMatcher {
    seqs: Box<[ElemSeq]>,
    facts: LiteralFacts,
    prefixes: Box<[Box<[u8]>]>,
    case_insensitive: bool,
    dot: bool,
}

impl SegmentMatcher {
    pub(crate) fn build(program: OpProgram, dot: bool) -> Result<Box<Self>, OpProgram> {
        let ci = program.case_insensitive;
        let Some(seqs) = compile::compile_seqs(&program.ops, dot, ci) else {
            return Err(program);
        };
        let prefixes = compute_static_prefixes(&program.ops);
        let facts = program.facts;
        Ok(Box::new(Self {
            seqs: seqs.into_boxed_slice(),
            facts,
            prefixes,
            case_insensitive: ci,
            dot,
        }))
    }

    pub(crate) fn static_prefixes(&self) -> &[Box<[u8]>] {
        &self.prefixes
    }

    pub(crate) fn is_match(&self, path: &[u8]) -> bool {
        if !self.facts.accept(path) {
            return false;
        }
        if self.seqs.len() == 1 {
            return self.seq_matches(&self.seqs[0], path);
        }
        self.seqs.iter().any(|seq| {
            let qs = &seq.quick_suffix;
            if !qs.is_empty() {
                let n = path.len();
                if n < qs.len() || !self.affix_eq(qs, &path[n - qs.len()..]) {
                    return false;
                }
            }
            self.seq_matches(seq, path)
        })
    }

    pub(crate) fn match_dir(&self, dir_path: &[u8]) -> DirMatch {
        if dir_path.is_empty() {
            return DirMatch::from_exact_prefix(self.is_match(dir_path), true);
        }
        let (mut exact, mut prefix) = (false, false);
        for seq in self.seqs.iter() {
            let (e, p) = self.seq_match_dir(seq, dir_path);
            exact |= e;
            prefix |= p;
            if exact && prefix {
                break;
            }
        }
        DirMatch::from_exact_prefix(exact, prefix)
    }
}
