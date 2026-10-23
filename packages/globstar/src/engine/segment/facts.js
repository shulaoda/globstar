// Literal facts read off the AST at compile time: the suffix prefilter
// and the static prefixes a walker seeds from. JS twin of
// `crates/globstar/src/engine/segment/facts.rs`.

import { hasEscapedSep } from "../../bytes.js";
import { N_BRACE, N_LITERAL, N_SEPARATOR, endsWithGlobstar, seq } from "../../parse/ast.js";
import { nodeWeight } from "./graph.js";

// Literal suffixes one of which every match ends with: the trailing
// literal/separator run (separators as `/`), or when the pattern ends in
// a brace, one per branch. Empty when nothing is guaranteed. One char
// per byte, like the path strings they are compared with.
export function suffixes(body) {
  const nodes = seq(body);
  const suffix = trailing(nodes, nodes.length);
  if (suffix.length > 0) return [suffix];
  return suffixSet(nodes);
}

// The bytes of the literal/separator run that ends `nodes[..end]`. The
// `/` of a `**/` is not part of it: `**/b` matches `b` alone.
function trailing(nodes, end) {
  let acc = "";
  for (let i = end - 1; i >= 0; i--) {
    const node = nodes[i];
    if (node.tag === N_LITERAL) acc = node.str + acc;
    else if (node.tag === N_SEPARATOR && !(i > 0 && endsWithGlobstar(nodes[i - 1])))
      acc = "/" + acc;
    else break;
  }
  return acc;
}

// Largest suffix set worth scanning before the engine runs; a bigger
// alternation gets no prefilter (the engine groups it).
const MAX_SUFFIX_SET = 16;

// When the pattern ends in a brace: one guaranteed suffix per branch, or
// none when a branch guarantees nothing.
function suffixSet(nodes) {
  if (nodes.length === 0) return [];
  const last = nodes[nodes.length - 1];
  if (last.tag !== N_BRACE || last.branches.length > MAX_SUFFIX_SET) return [];

  const commonTail = trailing(nodes, nodes.length - 1);

  const set = [];
  for (const branch of last.branches) {
    const b = seq(branch);
    const branchSuffix = trailing(b, b.length);
    // Gluing the common tail to a branch that is not all-literal would
    // claim more than the branch guarantees.
    const allLiteral = b.every((n) => n.tag === N_LITERAL || n.tag === N_SEPARATOR);
    const full = allLiteral ? commonTail + branchSuffix : branchSuffix;
    if (full.length === 0) return [];
    set.push(full);
  }
  return set;
}

// How deep brace expansions may nest along one static prefix, the part
// of its budget that does not depend on the pattern, and how many
// budgets of bytes recording the heads may cost.
const MAX_HEAD_DEPTH = 64;
const HEAD_BUDGET_FLOOR = 4096;
const HEAD_COPY_FACTOR = 64;

// The deepest segment-bounded literal head of each expansion,
// deduplicated, sorted by (length, bytes). A brace in the head is taken
// branch by branch with what follows it (`{a,b}/c` gives `a/c` and
// `b/c`), so a factored union keeps its members' heads.
//
// Every byte read and every branch taken costs one unit of a budget of
// HEAD_BUDGET_FLOOR plus twice the pattern's weight (`nodeWeight`, only
// worked out once the floor is spent): reading each node once costs at
// most its weight, and re-reading what follows a brace for each branch
// is what the rest pays for. Past the budget, or past MAX_HEAD_DEPTH
// nested braces, the head ends at its last whole segment, a shorter
// prefix that still holds: `{a,b}{c,d}…` stops after a few braces
// instead of doubling, and a brace before a long literal copies it a few
// times, not once per branch. Recording a head costs every byte read to
// reach it (the slice flattens them all): past HEAD_COPY_FACTOR budgets
// of those, the reading stops and starts again as if at the depth limit,
// which gives the one head before the first brace, shared by every
// expansion.
export function computeStaticPrefixes(body) {
  const nodes = seq(body);
  let heads = readAt(nodes, 0);
  if (heads.copied > HEAD_COPY_FACTOR * heads.limit) heads = readAt(nodes, MAX_HEAD_DEPTH);
  return dedupe(heads.out);
}

// Read the heads of the whole pattern `nodes`, from brace depth `depth`.
function readAt(nodes, depth) {
  const heads = {
    root: nodes,
    spent: 0,
    limit: HEAD_BUDGET_FLOOR,
    weighed: false,
    copied: 0,
    out: [],
  };
  readHeads(heads, nodes, 0, null, "", 0, depth);
  return heads;
}

// Read `nodes` from `at`, then `then` (`{ nodes, at, next }`: what
// follows them), after the bytes `acc` (whose last whole segment ends at
// `boundary`), and record the head each expansion reaches.
function readHeads(heads, nodes, at, then, acc, boundary, depth) {
  let i = at;
  let end;
  for (;;) {
    if (i === nodes.length) {
      if (then === null) {
        end = acc.length; // all literal
        break;
      }
      nodes = then.nodes;
      i = then.at;
      then = then.next;
      continue;
    }
    const node = nodes[i++];
    // An escaped `\` on Windows: the expansion matches nothing (§4.2), so
    // it has no head to seed.
    if (node.tag === N_LITERAL && hasEscapedSep(node.str)) return;
    if (node.tag === N_LITERAL && afford(heads, node.str.length)) {
      acc += node.str;
    } else if (node.tag === N_SEPARATOR && afford(heads, 1)) {
      acc += "/";
      boundary = acc.length;
    } else if (
      node.tag === N_BRACE &&
      depth < MAX_HEAD_DEPTH &&
      afford(heads, node.branches.length)
    ) {
      const after = { nodes, at: i, next: then };
      for (const branch of node.branches) {
        // An empty head covers every other one: nothing more to read.
        if (heads.out.length > 0 && heads.out[heads.out.length - 1].length === 0) break;
        readHeads(heads, seq(branch), 0, after, acc, boundary, depth + 1);
      }
      return;
    } else {
      // Every branch of a brace opening a new segment leaves what came
      // before whole.
      end = node.tag === N_BRACE && opensSegment(node) ? acc.length : boundary;
      break;
    }
  }
  while (end > 0 && acc.charCodeAt(end - 1) === 0x2f) end--;
  heads.copied += acc.length;
  if (within(heads, heads.copied, HEAD_COPY_FACTOR)) {
    heads.out.push(acc.slice(0, end));
  } else {
    // An empty head covers all the others: the reading stops.
    heads.out = [""];
  }
}

// Spend `n` units, unless that passes the limit.
function afford(heads, n) {
  if (!within(heads, heads.spent + n, 1)) return false;
  heads.spent += n;
  return true;
}

// Whether `used` stays within `factor` limits. The limit is the floor
// until `used` passes it, then the floor plus twice the weight.
function within(heads, used, factor) {
  if (used > factor * heads.limit && !heads.weighed) {
    heads.weighed = true;
    for (const node of heads.root) heads.limit += 2 * nodeWeight(node);
  }
  return used <= factor * heads.limit;
}

function opensSegment(node) {
  return node.branches.every((b) => seq(b)[0]?.tag === N_SEPARATOR);
}

// Drop prefixes a shorter one already covers; an empty one covers all.
// Shortest first, so a prefix is only ever covered by one kept before,
// found by a binary search at each of its `/`.
function dedupe(prefixes) {
  if (prefixes.length <= 1) return prefixes;
  prefixes.sort((a, b) => a.length - b.length || (a < b ? -1 : a > b ? 1 : 0));
  const kept = [];
  const covered = (p) => {
    for (let i = p.indexOf("/"); i !== -1; i = p.indexOf("/", i + 1)) {
      let lo = 0;
      let hi = kept.length;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        const k = kept[mid];
        const c = k.length - i || (p.startsWith(k) ? 0 : k < p ? -1 : 1);
        if (c === 0) return true;
        if (c < 0) lo = mid + 1;
        else hi = mid;
      }
    }
    return false;
  };
  for (const p of prefixes) {
    if (p.length === 0) return [p];
    if (p !== kept[kept.length - 1] && !covered(p)) kept.push(p);
  }
  return kept;
}
