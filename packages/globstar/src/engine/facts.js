import { isPathSep, eqByteCi } from "../options.js";
import { OP_LIT, OP_SEP, OP_ALTERNATION } from "./ops/ir.js";

export class LiteralFacts {
  constructor(suffix, suffixSet, caseInsensitive) {
    this.suffix = suffix;
    this.suffixSet = suffixSet;
    this.caseInsensitive = caseInsensitive;
  }

  static extract(ops, caseInsensitive) {
    const suffix = Uint8Array.from(suffixArray(ops, ops.length));
    const suffixSet = suffix.length === 0 ? extractSuffixSet(ops) : [];
    return new LiteralFacts(suffix, suffixSet, caseInsensitive);
  }

  accept(path) {
    if (this.suffix.length > 0) return this.endsWith(path, this.suffix);
    if (this.suffixSet.length === 0) return true;
    for (let i = 0; i < this.suffixSet.length; i++) {
      if (this.endsWith(path, this.suffixSet[i])) return true;
    }
    return false;
  }

  endsWith(path, suffix) {
    const ci = this.caseInsensitive;
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
    if (branchSuffix.length === 0 && branch.length > 0) return [];

    let allLiteral = true;
    for (const op of branch) {
      if (op.kind !== OP_LIT && op.kind !== OP_SEP) {
        allLiteral = false;
        break;
      }
    }

    const full = allLiteral
      ? Uint8Array.from(commonTail.concat(branchSuffix))
      : Uint8Array.from(branchSuffix);
    if (full.length === 0) return [];
    set.push(full);
  }
  return set;
}
