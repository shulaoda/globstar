// Suffix-anchored prefilter consulted by every engine's `isMatch`.
//
// Every `OpProgram` carries a `LiteralFacts` recording the byte suffix
// (or set of suffixes for trailing brace alternations) every matching
// path must end with. Before invoking the engine, the matcher
// short-circuits on a separator-aware `endsWith` check:
//
//   path ends with suffix  → maybe match (run engine)
//   otherwise              → definitely not (return false)
//
// Picks up the bulk of "wrong file extension" rejects on walker
// workloads — `**/*.ts` against a `.js` file is a single suffix scan,
// no NFA / DFA work.
//
// Correctness invariant: `accept(path) === false` ⇒ no program variant
// can match `path`. The filter must therefore never reject a path the
// engine would accept; this drives:
//   1. Conservative extraction — stop at any non-literal op.
//   2. Separator-aware compare — a `/` in the suffix matches any single
//      byte from the platform's `Seps` set (GLOB_SPEC §12.3).

import { isPathSep, eqByteCi } from "../options.js";

// Op kinds — duplicated from `./ops.js` to break the import cycle.
const OP_LIT = 0;
const OP_SEP = 4;
const OP_SEP_RUN = 5;
const OP_ALTERNATION = 11;

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

// Right-to-left scan: collect Lit / Sep bytes until the first
// non-literal op. Returns the guaranteed byte suffix of any match.
function extractSuffix(ops) {
  const acc = [];
  for (let i = ops.length - 1; i >= 0; i--) {
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
  return Uint8Array.from(acc);
}

// If the program ends with `Alternation` of literal-only branches, build
// one suffix per branch (e.g. `**/*.{ts,tsx,js}` → ["ts", "tsx", "js"]
// extended with the common tail "."). Returns [] when any branch is
// non-literal or the result would be empty (useless filter).
function extractSuffixSet(ops) {
  if (ops.length === 0) return [];
  const last = ops[ops.length - 1];
  if (last.kind !== OP_ALTERNATION) return [];

  // Tail literals BEFORE the alternation can be safely glued to each
  // all-literal branch.
  const commonTail = extractSuffix(ops.slice(0, ops.length - 1));

  const set = [];
  for (const branch of last.branches) {
    const branchSuffix = extractSuffix(branch);
    // (a) Branch contributes no literal at the tail (e.g. `{..Star}`)
    //     — abandon the suffix-set strategy entirely.
    if (branchSuffix.length === 0 && branch.length > 0) return [];

    // commonTail can only safely glue when the WHOLE branch is literal —
    // otherwise non-literal content sits between commonTail and the
    // branch tail and `commonTail + branchSuffix` is not a real suffix.
    let allLiteral = true;
    for (const op of branch) {
      if (op.kind !== OP_LIT && op.kind !== OP_SEP && op.kind !== OP_SEP_RUN) {
        allLiteral = false;
        break;
      }
    }

    let full;
    if (allLiteral) {
      full = new Uint8Array(commonTail.length + branchSuffix.length);
      full.set(commonTail, 0);
      full.set(branchSuffix, commonTail.length);
    } else {
      full = branchSuffix;
    }
    // (b) Empty final suffix — useless as a filter.
    if (full.length === 0) return [];
    set.push(full);
  }
  return set;
}

// Separator-aware `endsWith`. A `/` in `suffix` matches any single
// platform-separator byte in `path`. `path` is a `Uint8Array` (encoded
// at the public `compileMatcher` boundary).
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
