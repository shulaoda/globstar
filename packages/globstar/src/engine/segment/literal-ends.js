// The literal ends every match shares, read off the AST: the suffixes
// the prefilter checks and the static prefixes a walker seeds from.

import { hasEscapedSep } from "../../bytes.js";
import { N_BRACE, N_LITERAL, N_SEPARATOR, endsInGlobstar, seq } from "../../parse/ast.js";
import { nodeWeight } from "./graph.js";

// A pattern that ends in a brace of more branches gets no prefilter.
const MAX_SUFFIX_SET = 16;

// How deep braces may nest along one head, the budget every pattern gets
// for reading heads, and how many budgets of bytes recording them may cost.
const MAX_HEAD_DEPTH = 64;
const HEAD_BUDGET_FLOOR = 4096;
const HEAD_COPY_FACTOR = 64;

// Literal suffixes one of which every match ends with: the trailing
// literal run, or one per branch of a trailing brace. Empty when nothing
// is guaranteed.
export function suffixes(body) {
  const nodes = seq(body);
  const suffix = trailing(nodes, nodes.length, false);
  if (suffix.length > 0) return [suffix];
  return suffixSet(nodes);
}

// The literal/separator run that ends `nodes[..end]`. The `/` of a `**/`
// is not part of it, nor one a brace that may be empty parts from a `**`:
// `**/b` and `**{,a}/b` match `b` alone.
function trailing(nodes, end, afterGlobstar) {
  let acc = "";
  for (let i = end - 1; i >= 0; i--) {
    const node = nodes[i];
    if (node.tag === N_LITERAL) acc = node.str + acc;
    else if (node.tag === N_SEPARATOR && !endsInGlobstar(nodes, i, afterGlobstar)) acc = "/" + acc;
    else break;
  }
  return acc;
}

// One suffix per branch of a trailing brace, or none when a branch
// guarantees nothing.
function suffixSet(nodes) {
  if (nodes.length === 0) return [];
  const last = nodes[nodes.length - 1];
  if (last.tag !== N_BRACE || last.branches.length > MAX_SUFFIX_SET) return [];
  const common = trailing(nodes, nodes.length - 1, false);
  const afterGlobstar = endsInGlobstar(nodes, nodes.length - 1, false);
  const set = [];
  for (const branch of last.branches) {
    const b = seq(branch);
    const own = trailing(b, b.length, afterGlobstar);
    // The common tail joins a branch only when the branch is all literal;
    // otherwise it would claim more than the branch does.
    const allLiteral = b.every((n) => n.tag === N_LITERAL || n.tag === N_SEPARATOR);
    const full = allLiteral ? common + own : own;
    if (full.length === 0) return [];
    set.push(full);
  }
  return set;
}

// The deepest segment-bounded literal head of each brace expansion,
// deduplicated. Every byte read and branch taken costs one unit of a
// budget; past it, or past MAX_HEAD_DEPTH, a head ends at its last whole
// segment, a shorter prefix that still holds. Past HEAD_COPY_FACTOR
// budgets of bytes copied into heads, the reading starts again at the
// depth limit, which gives the one head before the first brace.
export function staticPrefixes(body) {
  const nodes = seq(body);
  let heads = readAt(nodes, 0);
  if (heads.copied > HEAD_COPY_FACTOR * heads.limit) heads = readAt(nodes, MAX_HEAD_DEPTH);
  return dedupe(heads.out);
}

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

// Read `nodes` from `at`, then `then` (`{ nodes, at, next }`), after `acc`
// (whose last whole segment ends at `boundary`), and record the head each
// expansion reaches.
function readHeads(heads, nodes, at, then, acc, boundary, depth) {
  let i = at;
  let end;
  for (;;) {
    if (i === nodes.length) {
      if (then === null) {
        end = acc.length;
        break;
      }
      nodes = then.nodes;
      i = then.at;
      then = then.next;
      continue;
    }
    const node = nodes[i++];
    // An expansion through an escaped `\` on Windows matches nothing
    // (GLOB_SPEC §4.2): no head to seed.
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
        if (covered(heads)) break;
        readHeads(heads, seq(branch), 0, after, acc, boundary, depth + 1);
      }
      return;
    } else {
      // Every branch opening a new segment leaves what came before whole.
      end = node.tag === N_BRACE && opensSegment(node) ? acc.length : boundary;
      break;
    }
  }
  while (end > 0 && acc.charCodeAt(end - 1) === 0x2f) end--;
  heads.copied += acc.length;
  if (within(heads, heads.copied, HEAD_COPY_FACTOR)) heads.out.push(acc.slice(0, end));
  else heads.out = [""];
}

// An empty head covers every other one.
const covered = (heads) => heads.out.length > 0 && heads.out[heads.out.length - 1].length === 0;

function afford(heads, n) {
  if (!within(heads, heads.spent + n, 1)) return false;
  heads.spent += n;
  return true;
}

// The limit is the floor until `used` passes it; the pattern's weight is
// added only then.
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

// Drops the prefixes a shorter one covers; an empty one covers all.
function dedupe(prefixes) {
  if (prefixes.length <= 1) return prefixes;
  prefixes.sort((a, b) => a.length - b.length || (a < b ? -1 : a > b ? 1 : 0));
  const kept = [];
  const coveredBy = (p) => {
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
    if (p !== kept[kept.length - 1] && !coveredBy(p)) kept.push(p);
  }
  return kept;
}
