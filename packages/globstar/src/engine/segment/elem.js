// The segment graph's vocabulary, shared by the compiler, the matcher
// and the executor. JS twin of the enums in
// `crates/globstar/src/engine/segment/mod.rs`.

// Paths through the graph `isMatch` tries one by one; past that the
// NFA over the whole graph runs.
export const MAX_CHAINS = 64;
// An affix set up to this size is scanned in pattern order; a larger
// one is sorted and binary-searched per suffix length.
export const LINEAR_SUFFIXES = 8;

// Element kinds.
export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2;
export const EL_G0_STRICT = 3;
export const EL_G1 = 4;

// In-segment wildcard kinds.
export const WK_AFFIX = 0;
export const WK_AFFIX_SET = 1;
export const WK_GENERIC = 2;
export const WK_AFFIX_INDEX = 3;

// `chain.globstar` when the chain has none / several.
export const NO_G = -1;
export const MANY_G = -2;

// Match results; BAIL is string mode giving up on a non-ASCII segment.
export const NO = 0;
export const YES = 1;
export const BAIL = 2;

// Can the element take some segment?
export const elemSatisfiable = (e) =>
  e.kind !== EL_WILD || e.wild.kind !== WK_GENERIC || e.wild.nfa.satisfiable;
// Takes every non-empty segment (`matchDir`'s all-below).
export const elemAnySeg = (e) =>
  e.kind === EL_LIT ? false : e.kind === EL_WILD ? e.wild.anySeg : true;
