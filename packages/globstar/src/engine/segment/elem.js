// The segment graph's vocabulary, shared by the compiler, the matcher
// and the executor. JS twin of the enums in
// `crates/globstar/src/engine/segment/{mod,wild,nfa}.rs`.

// Element kinds.
export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2;
export const EL_G1 = 3;

export const isGlobstar = (e) => e.kind === EL_G0 || e.kind === EL_G1;
// Absorbs zero segments too.
export const nullable = (e) => e.kind === EL_G0;

// In-segment wildcard kinds.
export const WK_AFFIX = 0;
export const WK_AFFIX_SET = 1;
export const WK_GENERIC = 2;
export const WK_AFFIX_INDEX = 3;

// Graph NFA kinds: dense int32 rows, sparse.
export const NFA_SMALL = 0;
export const NFA_SPARSE = 1;

// The `minLen` of a segment no path segment can fill: one with an
// escaped `\` on Windows, where a segment never holds a separator, or an
// in-segment NFA that accepts nothing.
export const NEVER_LEN = 0x7fffffff;

// Can the element take some segment?
export const elemSatisfiable = (e) => e.kind !== EL_WILD || e.wild.minLen !== NEVER_LEN;
// Takes every non-empty segment (`matchDir`'s all-below).
export const elemAnySeg = (e) =>
  e.kind === EL_LIT ? false : e.kind === EL_WILD ? e.wild.anySeg : true;

// `a` copied to its length. V8 grows an array that `push` fills by half
// again plus 16 slots and never gives the room back; a matcher keeps its
// arrays as long as it lives, so the ones it stores are copied once.
export function exact(a) {
  return a.slice();
}
