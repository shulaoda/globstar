export const MATCH = 0;
export const PRUNED = 1;
export const DESCEND = 2;
export const DESCEND_AND_MATCH = 3;
export const DESCEND_ALL = 4;
export const DESCEND_ALL_AND_MATCH = 5;

export const DirMatch = {
  Match: MATCH,
  Pruned: PRUNED,
  Descend: DESCEND,
  DescendAndMatch: DESCEND_AND_MATCH,
  DescendAll: DESCEND_ALL,
  DescendAllAndMatch: DESCEND_ALL_AND_MATCH,

  isMatch(d) {
    return d === MATCH || d === DESCEND_AND_MATCH || d === DESCEND_ALL_AND_MATCH;
  },

  shouldDescend(d) {
    return d >= DESCEND && d <= DESCEND_ALL_AND_MATCH;
  },

  isPruned(d) {
    return d === PRUNED;
  },

  // Every path below the directory matches: `match(dir + "/" + s)` holds
  // for every `s` of one or more non-empty segments. `true` is a
  // guarantee; `false` means "not all, or not provable".
  matchesAllBelow(d) {
    return d === DESCEND_ALL || d === DESCEND_ALL_AND_MATCH;
  },

  fromExactPrefix(exact, prefix) {
    if (exact && prefix) return DESCEND_AND_MATCH;
    if (exact) return MATCH;
    if (prefix) return DESCEND;
    return PRUNED;
  },

  // `fromExactPrefix` plus the all-below axis, which implies `prefix`.
  fromExactPrefixAll(exact, prefix, all) {
    if (all) return exact ? DESCEND_ALL_AND_MATCH : DESCEND_ALL;
    return DirMatch.fromExactPrefix(exact, prefix);
  },
};
