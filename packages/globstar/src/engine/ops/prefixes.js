import { OP_ALTERNATION, OP_LIT, OP_SEP, OP_SEP_RUN, OP_SLASH_ANYTHING } from "./ir.js";

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
      // Strict trailing `/**` consumes a separator before matching anything,
      // so the accumulated literal is a complete segment.
      if (op.kind === OP_SLASH_ANYTHING) lastBoundary = acc.length;
      fullyLiteral = false;
      break;
    }
  }
  let length = fullyLiteral ? acc.length : lastBoundary;
  while (length > 0 && acc[length - 1] === 0x2f) length--;
  return String.fromCharCode(...acc.slice(0, length));
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
