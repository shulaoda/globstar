// Suffix-anchored prefilter run before every engine's `isMatch`.
//
// Every `OpProgram` carries a `LiteralFacts` recording the byte suffix (or a
// set of them, for a trailing brace alternation) that every matching path
// must end with. The matcher checks it with a separator-aware `endsWith`
// before running the engine:
//
//   path ends with suffix  →  maybe a match, run the engine
//   otherwise              →  definitely not
//
// On walker workloads this rejects most candidates outright. `**/*.ts` drops
// every `.js` file in one suffix scan. Only the suffix is recorded, not a
// prefix, because the engines already scan left to right and the tail is the
// one anchor they can't check up front.
//
// Correctness invariant. `accept(path) === false` must mean no program
// variant can match `path`, so the filter must never reject a path the engine
// would accept. Two rules keep it safe.
//   1. Conservative extraction. Stop at the first non-literal op.
//   2. Separator-aware compare. A `/` in the suffix matches any single
//      separator byte, `/` or `\` (GLOB_SPEC §12.3).

import { isPathSep, eqByteCi } from "../options.js";
import { OP_LIT, OP_SEP, OP_SEP_RUN, OP_ALTERNATION } from "./ops/ir.js";

export class LiteralFacts {
  constructor(suffix, suffixSet, caseInsensitive) {
    this.suffix = suffix; // Uint8Array (length 0 = no fact)
    this.suffixSet = suffixSet; // Array<Uint8Array> (one entry per brace branch)
    this.caseInsensitive = caseInsensitive;
  }

  static extract(ops, caseInsensitive) {
    const suffix = extractSuffix(ops);
    const suffixSet = suffix.length === 0 ? extractSuffixSet(ops) : [];
    return new LiteralFacts(suffix, suffixSet, caseInsensitive);
  }

  accept(path) {
    const ci = this.caseInsensitive;
    if (this.suffix.length > 0) return endsWith(path, this.suffix, ci);
    if (this.suffixSet.length === 0) return true;
    for (let i = 0; i < this.suffixSet.length; i++) {
      if (endsWith(path, this.suffixSet[i], ci)) return true;
    }
    return false;
  }
}

// Walk ops right-to-left, collecting Lit and Sep bytes until the first
// non-literal op. Returns the byte suffix every match must end with.
function extractSuffix(ops) {
  return Uint8Array.from(suffixArray(ops, ops.length));
}

// Plain-array core shared by `extractSuffix` and the suffix-set builder.
// Scans `ops[0 .. end)`.
function suffixArray(ops, end) {
  const acc = [];
  for (let i = end - 1; i >= 0; i--) {
    const op = ops[i];
    if (op.kind === OP_LIT) {
      for (let j = op.bytes.length - 1; j >= 0; j--) acc.push(op.bytes[j]);
    } else if (op.kind === OP_SEP || op.kind === OP_SEP_RUN) {
      acc.push(0x2f);
    } else {
      break;
    }
  }
  acc.reverse();
  return acc;
}

// When the program ends in an `Alternation` of literal-only branches, build
// one suffix per branch. `**/*.{ts,tsx,js}` glues the common tail "." onto
// each to give [".ts", ".tsx", ".js"]. Returns [] when a branch is
// non-literal or the result would be empty.
function extractSuffixSet(ops) {
  if (ops.length === 0) return [];
  const last = ops[ops.length - 1];
  if (last.kind !== OP_ALTERNATION) return [];

  // Tail literals BEFORE the alternation can be safely glued to each
  // all-literal branch.
  const commonTail = suffixArray(ops, ops.length - 1);

  const set = [];
  for (const branch of last.branches) {
    const branchSuffix = suffixArray(branch, branch.length);
    // (a) Branch has no literal tail (e.g. `{..Star}`), so no reliable
    //     suffix. Abandon the set.
    if (branchSuffix.length === 0 && branch.length > 0) return [];

    // commonTail is only safe to glue when the whole branch is literal.
    // Otherwise non-literal content sits between it and the branch tail, so
    // `commonTail + branchSuffix` is not a real suffix.
    let allLiteral = true;
    for (const op of branch) {
      if (op.kind !== OP_LIT && op.kind !== OP_SEP && op.kind !== OP_SEP_RUN) {
        allLiteral = false;
        break;
      }
    }

    const full = allLiteral
      ? Uint8Array.from(commonTail.concat(branchSuffix))
      : Uint8Array.from(branchSuffix);
    // (b) Empty final suffix, useless as a filter.
    if (full.length === 0) return [];
    set.push(full);
  }
  return set;
}

// Separator-aware `endsWith`. A `/` in `suffix` matches any single separator
// byte in `path` (`/` always, `\` on Windows).
function endsWith(path, suffix, ci) {
  let si = suffix.length;
  let pi = path.length;
  while (si > 0) {
    if (pi === 0) return false;
    si--;
    pi--;
    const sb = suffix[si];
    const pb = path[pi];
    if (sb === 0x2f) {
      if (!isPathSep(pb)) return false;
    } else if (ci ? !eqByteCi(sb, pb) : sb !== pb) {
      return false;
    }
  }
  return true;
}
