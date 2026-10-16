// Literal facts read off the ops at compile time: the suffix prefilter
// and the static prefixes a walker seeds from. JS twin of
// `crates/globstar/src/engine/segment/facts.rs`.

import { isPathSep, eqByteCi } from "../../bytes.js";
import { latin1 } from "../../utf8.js";
import { OP_ALTERNATION, OP_LIT, OP_SEP, OP_SEP_RUN, OP_SLASH_ANYTHING } from "../ops.js";
import { opWeight } from "./compile.js";

// Literal suffixes one of which every match ends with: the trailing
// `Lit`/`Sep` run (separators as `/`), or when the pattern ends in a
// brace, one per branch. Empty when nothing is guaranteed. One char per
// byte, like the path strings they are compared with.
export function suffixes(ops) {
  const suffix = suffixArray(ops, ops.length);
  if (suffix.length > 0) return [latin1(suffix)];
  return extractSuffixSet(ops);
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

    const full = latin1(allLiteral ? commonTail.concat(branchSuffix) : branchSuffix);
    if (full.length === 0) return [];
    set.push(full);
  }
  return set;
}

// How deep brace expansions may nest along one static prefix, and the
// part of its budget that does not depend on the pattern.
const MAX_HEAD_DEPTH = 64;
const HEAD_BUDGET_FLOOR = 4096;

// The deepest segment-bounded literal head of each expansion,
// deduplicated, sorted by (length, bytes). A brace in the head is taken
// branch by branch with what follows it (`{a,b}/c` gives `a/c` and
// `b/c`), so a factored union keeps its members' heads.
//
// Every byte read and every branch taken costs one unit of a budget of
// HEAD_BUDGET_FLOOR plus twice the pattern's weight (`opWeight`, only
// worked out once the floor is spent): reading each op once costs at
// most its weight, and re-reading what follows a brace for each branch
// is what the rest pays for. Past the budget, or past MAX_HEAD_DEPTH
// nested braces, the head ends at its last whole segment, a shorter
// prefix that still holds: `{a,b}{c,d}…` stops after a few braces
// instead of doubling, and a brace before a long literal copies it a few
// times, not once per branch.
export function computeStaticPrefixes(ops) {
  const heads = { root: ops, spent: 0, limit: HEAD_BUDGET_FLOOR, weighed: false, out: [] };
  readHeads(heads, ops, 0, null, [], 0, 0);
  return dedupePrefixes(heads.out);
}

// Read `ops` from `at`, then `then` (`{ ops, at, next }`: what follows
// them), after the bytes `acc` (whose last whole segment ends at
// `boundary`), and record the head each expansion reaches.
function readHeads(heads, ops, at, then, acc, boundary, depth) {
  const mark = acc.length;
  let i = at;
  let end;
  for (;;) {
    if (i === ops.length) {
      if (then === null) {
        end = acc.length; // all literal
        break;
      }
      ops = then.ops;
      i = then.at;
      then = then.next;
      continue;
    }
    const op = ops[i++];
    if (op.kind === OP_LIT && afford(heads, op.bytes.length)) {
      for (let k = 0; k < op.bytes.length; k++) acc.push(op.bytes[k]);
    } else if ((op.kind === OP_SEP || op.kind === OP_SEP_RUN) && afford(heads, 1)) {
      acc.push(0x2f);
      boundary = acc.length;
    } else if (op.kind === OP_SLASH_ANYTHING) {
      // `/**`: the segment before it is whole.
      end = acc.length;
      break;
    } else if (
      op.kind === OP_ALTERNATION &&
      depth < MAX_HEAD_DEPTH &&
      afford(heads, op.branches.length)
    ) {
      const after = { ops, at: i, next: then };
      for (const branch of op.branches) {
        // An empty head covers every other one: nothing more to read.
        if (heads.out.length > 0 && heads.out[heads.out.length - 1].length === 0) break;
        readHeads(heads, branch, 0, after, acc, boundary, depth + 1);
      }
      acc.length = mark;
      return;
    } else {
      // Every branch of an alternation opening a new segment leaves what
      // came before whole.
      end = op.kind === OP_ALTERNATION && opensSegment(op) ? acc.length : boundary;
      break;
    }
  }
  while (end > 0 && acc[end - 1] === 0x2f) end--;
  heads.out.push(latin1(acc.slice(0, end)));
  acc.length = mark;
}

// Spend `n` units, unless that passes the limit (the floor until it is
// spent, then the floor plus twice the weight).
function afford(heads, n) {
  if (heads.spent + n > heads.limit && !heads.weighed) {
    heads.weighed = true;
    for (const op of heads.root) heads.limit += 2 * opWeight(op);
  }
  if (heads.spent + n > heads.limit) return false;
  heads.spent += n;
  return true;
}

function opensSegment(alt) {
  return alt.branches.every((b) => {
    const k = b[0]?.kind;
    return k === OP_SEP || k === OP_SEP_RUN || k === OP_SLASH_ANYTHING;
  });
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
