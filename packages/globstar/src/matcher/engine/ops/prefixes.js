// Static path-prefix analysis used by the walker.

import { OP_ALTERNATION, OP_LIT, OP_SEP, OP_SEP_RUN } from "./ir.js";

export function computeStaticPrefixes(ops) {
  return dedupePrefixes(extractPrefixesPerBranch(ops));
}

function extractPrefixesPerBranch(ops) {
  if (ops.length > 0 && ops[0].kind === OP_ALTERNATION) {
    const out = [];
    for (const branch of ops[0].branches) {
      for (const prefix of extractPrefixesPerBranch(branch)) out.push(prefix);
    }
    return out;
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
      fullyLiteral = false;
      break;
    }
  }
  let length = fullyLiteral ? acc.length : lastBoundary;
  while (length > 0 && acc[length - 1] === 0x2f) length--;
  return Uint8Array.from(acc.slice(0, length));
}

export function dedupePrefixes(prefixes) {
  // Parents precede descendants. A byte trie answers exact and directory-
  // boundary ancestor queries in O(prefix length), replacing the previous
  // O(number of accepted prefixes) scan for every candidate.
  prefixes.sort((a, b) => a.length - b.length || compareBytes(a, b));
  const root = { terminal: false, children: new Map() };
  const result = [];
  for (const prefix of prefixes) {
    let node = root;
    let covered = node.terminal;
    let complete = true;
    for (let i = 0; !covered && i < prefix.length; i++) {
      if (prefix[i] === 0x2f && node.terminal) {
        covered = true;
        break;
      }
      const next = node.children.get(prefix[i]);
      if (next === undefined) {
        complete = false;
        break;
      }
      node = next;
    }
    if (covered || (complete && node.terminal)) continue;

    node = root;
    for (const byte of prefix) {
      let next = node.children.get(byte);
      if (next === undefined) {
        next = { terminal: false, children: new Map() };
        node.children.set(byte, next);
      }
      node = next;
    }
    node.terminal = true;
    result.push(prefix);
  }
  return result;
}

function compareBytes(a, b) {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return a.length - b.length;
}
