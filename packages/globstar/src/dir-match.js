export const MATCH = 0;
export const PRUNED = 1;
export const DESCEND = 2;
export const DESCEND_AND_MATCH = 3;

export const DirMatch = {
  Match: MATCH,
  Pruned: PRUNED,
  Descend: DESCEND,
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

  fromExactPrefix(exact, prefix) {
    if (exact && prefix) return DESCEND_AND_MATCH;
    if (exact) return MATCH;
    if (prefix) return DESCEND;
    return PRUNED;
  },
};
