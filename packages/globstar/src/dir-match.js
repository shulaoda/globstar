export const MATCH = 0;
export const PRUNED = 1;
export const DESCEND = 2;
export const DESCEND_AND_MATCH = 3;
export const DESCEND_ALL = 4;
export const DESCEND_ALL_AND_MATCH = 5;

export const DirMatch = Object.freeze({
  Match: MATCH,
  Pruned: PRUNED,
  Descend: DESCEND,
  DescendAndMatch: DESCEND_AND_MATCH,
  DescendAll: DESCEND_ALL,
  DescendAllAndMatch: DESCEND_ALL_AND_MATCH,

  from(matches, mayBelow, allBelow) {
    if (allBelow) return matches ? DESCEND_ALL_AND_MATCH : DESCEND_ALL;
    if (mayBelow) return matches ? DESCEND_AND_MATCH : DESCEND;
    return matches ? MATCH : PRUNED;
  },

  matches(d) {
    return d === MATCH || d === DESCEND_AND_MATCH || d === DESCEND_ALL_AND_MATCH;
  },

  mayMatchBelow(d) {
    return d >= DESCEND;
  },

  matchesAllBelow(d) {
    return d === DESCEND_ALL || d === DESCEND_ALL_AND_MATCH;
  },
});
