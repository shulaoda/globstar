// Result of a matcher's `matchDir(dirPath)`. Walkers consult this
// per-directory to decide whether to yield the dir, descend, or prune
// the subtree. Mirrors the Rust crate's `DirMatch` (ADR-005).

const PRUNED = 0;
const DESCEND = 1;
const MATCH = 2;
const DESCEND_AND_MATCH = 3;

export const DirMatch = {
  Pruned: PRUNED,
  Descend: DESCEND,
  Match: MATCH,
  DescendAndMatch: DESCEND_AND_MATCH,

  isMatch(d) {
    return d === MATCH || d === DESCEND_AND_MATCH;
  },
  shouldDescend(d) {
    return d === DESCEND || d === DESCEND_AND_MATCH;
  },
  isPruned(d) {
    return d === PRUNED;
  },

  // Combine the two boolean axes the engines compute internally.
  fromExactPrefix(exact, prefix) {
    if (exact && prefix) return DESCEND_AND_MATCH;
    if (exact) return MATCH;
    if (prefix) return DESCEND;
    return PRUNED;
  },
};
