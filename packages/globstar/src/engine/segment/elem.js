// The segment graph's vocabulary, shared by the compiler, the matcher
// and the executor. JS twin of `crates/globstar/src/engine/segment/
// elem.rs` and of the kinds in `{wild,nfa}.rs`.

// Element kinds.
export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2;
export const EL_G1 = 3;
// Takes no segment: it stands for its successors, which the tails of a
// group share through it instead of an edge from each (see `graph.js`).
export const EL_JOIN = 4;
// A piece of a segment a brace cuts, or a join among pieces: the glue NFA
// matches it with the pieces glued to it (see `GlueNfa`). Its `entry` is
// its entry state there when a segment may start in it, else INNER.
export const EL_PIECE = 5;

// The entry of a piece no segment starts in.
export const INNER = -1;

export const isGlobstar = (e) => e.kind === EL_G0 || e.kind === EL_G1;
// Absorbs zero segments too.
export const nullable = (e) => e.kind === EL_G0 || e.kind === EL_JOIN;

// In-segment wildcard kinds.
export const WK_AFFIX = 0;
export const WK_SUFFIXES = 1;
export const WK_GENERIC = 2;

// Graph NFA kinds: dense int32 rows, sparse.
export const NFA_SMALL = 0;
export const NFA_SPARSE = 1;

// The `minLen` of a segment no path segment can fill: one with an
// escaped `\` on Windows, where a segment never holds a separator, or an
// in-segment NFA that accepts nothing.
export const NEVER_LEN = 0x7fffffff;

// Can the element take some segment? (A piece's answer is the glue
// NFA's, `GlueNfa.reach`.)
export const elemSatisfiable = (e) =>
  e.kind === EL_WILD ? e.wild.minLen !== NEVER_LEN : e.kind !== EL_JOIN && e.kind !== EL_PIECE;
// Takes every non-empty segment (`matchDir`'s all-below).
export const elemAnySeg = (e) =>
  e.kind === EL_WILD ? e.wild.anySeg : e.kind === EL_G0 || e.kind === EL_G1;

// `a` copied to its length. V8 grows an array that `push` fills by half
// again plus 16 slots and never gives the room back; a matcher keeps its
// arrays as long as it lives, so the ones it stores are copied once.
export function exact(a) {
  return a.slice();
}
