// Literal facts read off the ops at compile time: the suffix prefilter
// and the static prefixes a walker seeds from. JS twin of
// `crates/globstar/src/engine/segment/facts.rs`.

import { isPathSep, eqByteCi } from "../../bytes.js";
import { latin1 } from "../../utf8.js";
import { OP_ALTERNATION, OP_LIT, OP_SEP, OP_SEP_RUN, OP_SLASH_ANYTHING } from "../ops.js";

// The literal suffixes a match must end with: one, or one per branch
// of a trailing brace (`suffixSet`), or neither. The segment matcher's
// prefilter reads them.
export class LiteralFacts {
  constructor(suffix, suffixSet) {
    this.suffix = suffix;
    this.suffixSet = suffixSet;
  }

  static extract(ops) {
    // One char per byte, like the path strings they are compared with.
    const suffix = String.fromCharCode(...suffixArray(ops, ops.length));
    const suffixSet = suffix.length === 0 ? extractSuffixSet(ops) : [];
    return new LiteralFacts(suffix, suffixSet);
  }
}

// Does `str` end with `suffix`? A pattern `/` takes any separator byte.
export function endsWithSepAware(str, suffix, ci) {
  let si = suffix.length;
  let pi = str.length;
  while (si > 0) {
    if (pi === 0) return false;
    si--;
    pi--;
    const sb = suffix.charCodeAt(si);
    const pb = str.charCodeAt(pi);
    if (sb === 0x2f) {
      if (!isPathSep(pb)) return false;
    } else if (ci ? !eqByteCi(sb, pb) : sb !== pb) {
      return false;
    }
  }
  return true;
}

function suffixArray(ops, end) {
  const acc = [];
  for (let i = end - 1; i >= 0; i--) {
    const op = ops[i];
    if (op.kind === OP_LIT) {
      for (let j = op.bytes.length - 1; j >= 0; j--) acc.push(op.bytes[j]);
    } else if (op.kind === OP_SEP) {
      acc.push(0x2f);
    } else {
      break;
    }
  }
  acc.reverse();
  return acc;
}

// Largest suffix set worth scanning before the engine runs; a bigger
// alternation gets no prefilter (the engine indexes it).
const MAX_SUFFIX_SET = 16;

function extractSuffixSet(ops) {
  if (ops.length === 0) return [];
  const last = ops[ops.length - 1];
  if (last.kind !== OP_ALTERNATION || last.branches.length > MAX_SUFFIX_SET) return [];

  const commonTail = suffixArray(ops, ops.length - 1);

  const set = [];
  for (const branch of last.branches) {
    const branchSuffix = suffixArray(branch, branch.length);
    let allLiteral = true;
    for (const op of branch) {
      if (op.kind !== OP_LIT && op.kind !== OP_SEP) {
        allLiteral = false;
        break;
      }
    }

    const full = allLiteral
      ? String.fromCharCode(...commonTail.concat(branchSuffix))
      : String.fromCharCode(...branchSuffix);
    if (full.length === 0) return [];
    set.push(full);
  }
  return set;
}

// The deepest segment-bounded literal prefix per top-level brace branch,
// deduplicated, sorted by (length, bytes).
export function computeStaticPrefixes(ops) {
  return dedupePrefixes(extractPrefixesPerBranch(ops));
}

function extractPrefixesPerBranch(ops) {
  if (ops.length > 0 && ops[0].kind === OP_ALTERNATION) {
    const next = ops[1];
    if (
      next === undefined ||
      next.kind === OP_SEP ||
      next.kind === OP_SEP_RUN ||
      next.kind === OP_SLASH_ANYTHING
    ) {
      const out = [];
      for (const branch of ops[0].branches) {
        for (const prefix of extractPrefixesPerBranch(branch)) out.push(prefix);
      }
      return out;
    }
  }
  return [extractLeadingPrefix(ops)];
}

function extractLeadingPrefix(ops) {
  const acc = [];
  let lastBoundary = 0;
  let fullyLiteral = true;
  for (const op of ops) {
    if (op.kind === OP_LIT) {
      for (let i = 0; i < op.bytes.length; i++) acc.push(op.bytes[i]);
    } else if (op.kind === OP_SEP || op.kind === OP_SEP_RUN) {
      acc.push(0x2f);
      lastBoundary = acc.length;
    } else {
      // Every branch of an alternation opening a new segment leaves what
      // came before whole, like a trailing `/**`.
      if (op.kind === OP_SLASH_ANYTHING || (op.kind === OP_ALTERNATION && opensSegment(op))) {
        lastBoundary = acc.length;
      }
      fullyLiteral = false;
      break;
    }
  }
  let length = fullyLiteral ? acc.length : lastBoundary;
  while (length > 0 && acc[length - 1] === 0x2f) length--;
  acc.length = length;
  // latin1 chunks fromCharCode; a spread would overflow on ~64k bytes.
  return latin1(Uint8Array.from(acc));
}

function opensSegment(alt) {
  return alt.branches.every((b) => b[0]?.kind === OP_SEP || b[0]?.kind === OP_SEP_RUN);
}

function dedupePrefixes(prefixes) {
  if (prefixes.length <= 1) return prefixes;
  const order = (a, b) => a.length - b.length || (a < b ? -1 : a > b ? 1 : 0);
  prefixes.sort(order);
  const accepted = new Set();
  for (const prefix of prefixes) {
    if (prefix.length === 0) return [prefix];
    let covered = false;
    for (let i = 0; !covered && i < prefix.length; i++) {
      if (prefix.charCodeAt(i) === 0x2f && accepted.has(prefix.slice(0, i))) covered = true;
    }
    if (!covered) accepted.add(prefix);
  }
  return [...accepted].sort(order);
}
