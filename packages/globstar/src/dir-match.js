// What a pattern says about a directory: whether its own path matches,
// and what can match below it. A walker reads the three facts, not the
// names: `matchesAllBelow` is a guarantee, `mayMatchBelow` errs toward true.

export const MATCH = 0;
export const PRUNED = 1;
export const DESCEND = 2;
export const DESCEND_AND_MATCH = 3;
export const DESCEND_ALL = 4;
export const DESCEND_ALL_AND_MATCH = 5;

export const DirMatch = Object.freeze({
  /** The directory matches; nothing below it can. */
  Match: MATCH,
  /** Neither the directory nor anything below it matches. */
  Pruned: PRUNED,
  /** The directory does not match; something below it may. */
  Descend: DESCEND,
  /** The directory matches, and something below it may. */
  DescendAndMatch: DESCEND_AND_MATCH,
  /** The directory does not match; everything below it does. */
  DescendAll: DESCEND_ALL,
  /** The directory matches, and so does everything below it. */
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

  /** `true`: every path of one or more non-empty segments below it matches. */
  matchesAllBelow(d) {
    return d === DESCEND_ALL || d === DESCEND_ALL_AND_MATCH;
  },
});
