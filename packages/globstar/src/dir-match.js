// The four-way result of `matchDir`, which lets a walker prune whole
// subtrees it never has to enter. Mirrors the Rust crate's `DirMatch`.

// Exported so consumers can compare a `matchDir` result against a named
// constant. Order mirrors the Rust enum (`Match` first).
export const MATCH = 0;
export const PRUNED = 1;
export const DESCEND = 2;
export const DESCEND_AND_MATCH = 3;

export const DirMatch = {
  Match: MATCH,
  Pruned: PRUNED,
  Descend: DESCEND,
  DescendAndMatch: DESCEND_AND_MATCH,

  // Whether the directory itself should be yielded as a match.
  isMatch(d) {
    return d === MATCH || d === DESCEND_AND_MATCH;
  },

  // Whether the walker should descend into the directory.
  shouldDescend(d) {
    return d === DESCEND || d === DESCEND_AND_MATCH;
  },

  // Whether the whole subtree can be skipped.
  isPruned(d) {
    return d === PRUNED;
  },

  // Assemble a DirMatch from the two questions each engine's matchDir
  // answers about a directory `d`:
  //
  // - `exact`  — does `d` itself match the pattern?
  // - `prefix` — could something below `d` match?
  //
  // e.g. for `src/**/*.rs`, dir `src` is `(exact: false, prefix: true)`
  // → `Descend`; dir `src/a.rs` is `(true, false)` → `Match`.
  fromExactPrefix(exact, prefix) {
    if (exact && prefix) return DESCEND_AND_MATCH;
    if (exact) return MATCH;
    if (prefix) return DESCEND;
    return PRUNED;
  },
};
