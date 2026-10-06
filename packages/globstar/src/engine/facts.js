import { isPathSep, eqByteCi } from "../bytes.js";
import { OP_LIT, OP_SEP, OP_ALTERNATION } from "./ops/ir.js";

export class LiteralFacts {
  constructor(suffix, suffixSet, caseInsensitive) {
    this.suffix = suffix;
    this.suffixSet = suffixSet;
    this.caseInsensitive = caseInsensitive;
  }

  static extract(ops, caseInsensitive) {
    // One char per byte, like the path strings they are compared with.
    const suffix = String.fromCharCode(...suffixArray(ops, ops.length));
    const suffixSet = suffix.length === 0 ? extractSuffixSet(ops) : [];
    return new LiteralFacts(suffix, suffixSet, caseInsensitive);
  }

  // `str` holds one char per path byte (see `utf8Latin1`).
  accept(str) {
    const ci = this.caseInsensitive;
    if (this.suffix.length > 0) return endsWithSepAware(str, this.suffix, ci);
    if (this.suffixSet.length === 0) return true;
    for (let i = 0; i < this.suffixSet.length; i++) {
      if (endsWithSepAware(str, this.suffixSet[i], ci)) return true;
    }
    return false;
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

function extractSuffixSet(ops) {
  if (ops.length === 0) return [];
  const last = ops[ops.length - 1];
  if (last.kind !== OP_ALTERNATION) return [];

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
