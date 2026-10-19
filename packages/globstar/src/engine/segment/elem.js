// The segment graph's vocabulary, shared by the compiler, the matcher
// and the executor. JS twin of the enums in
// `crates/globstar/src/engine/segment/mod.rs`.

// Paths through the graph `isMatch` tries one by one; past that the
// NFA over the whole graph runs.
export const MAX_CHAINS = 64;

// Element kinds.
export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2;
export const EL_G1 = 3;

// In-segment wildcard kinds.
export const WK_AFFIX = 0;
export const WK_AFFIX_SET = 1;
export const WK_GENERIC = 2;
export const WK_AFFIX_INDEX = 3;

// `chain.globstar` when the chain has none / several.
export const NO_G = -1;
export const MANY_G = -2;

// Graph NFA kinds: dense int32 rows, sparse.
export const NFA_SMALL = 0;
export const NFA_SPARSE = 1;

// The `minLen` of a segment no path segment can fill: one with an
// escaped `\` on Windows, where a segment never holds a separator.
export const NEVER_LEN = 0x7fffffff;

// Can the element take some segment?
export const elemSatisfiable = (e) =>
  e.kind !== EL_WILD ||
  (e.wild.minLen !== NEVER_LEN && (e.wild.kind !== WK_GENERIC || e.wild.nfa.satisfiable));
// Takes every non-empty segment (`matchDir`'s all-below).
export const elemAnySeg = (e) =>
  e.kind === EL_LIT ? false : e.kind === EL_WILD ? e.wild.anySeg : true;

// `a` copied to its length. V8 grows an array that `push` fills by half
// again plus 16 slots and never gives the room back; a matcher keeps its
// arrays as long as it lives, so the ones it stores are copied once.
export function exact(a) {
  return a.slice();
}
