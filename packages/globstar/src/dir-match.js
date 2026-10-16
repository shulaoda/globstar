// What a pattern says about a directory: whether the directory's own
// path matches, and what can match below it. JS twin of the Rust
// `DirMatch`.
//
// The names read from the pattern's side: the pattern matches the
// directory, descends below it (something below may match), or is
// pruned there (nothing at or below can match). What a walker does with
// that depends on the role the pattern plays, so the walker reads the
// three facts, not the names. A pattern that selects files is worth
// walking into a directory while `mayMatchBelow` holds; a pattern that
// excludes files lets the walker skip a directory whole once
// `matchesAllBelow` holds, and needs no more tests below a directory
// once `mayMatchBelow` is false. `matchesAllBelow` is a guarantee;
// `mayMatchBelow` errs toward true.

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

  // The verdict with these facts; `allBelow` implies `mayBelow`.
  from(matches, mayBelow, allBelow) {
    if (allBelow) return matches ? DESCEND_ALL_AND_MATCH : DESCEND_ALL;
    if (mayBelow) return matches ? DESCEND_AND_MATCH : DESCEND;
    return matches ? MATCH : PRUNED;
  },

  // Does the directory's own path match?
  matches(d) {
    return d === MATCH || d === DESCEND_AND_MATCH || d === DESCEND_ALL_AND_MATCH;
  },

  // May some path below the directory match? `false` is a guarantee
  // that none does.
  mayMatchBelow(d) {
    return d >= DESCEND;
  },

  // Does every path below the directory match: `match(dir + "/" + s)`
  // for every `s` of one or more non-empty segments? `true` is a
  // guarantee; `false` means "not all, or not provable".
  matchesAllBelow(d) {
    return d === DESCEND_ALL || d === DESCEND_ALL_AND_MATCH;
  },
});
