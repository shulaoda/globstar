// Which `**` are globstars (GLOB_SPEC §10.1, §9.1).
//
// A pattern means the union of its brace expansions, and in each one a `**`
// is a globstar only with a separator, or the edge of the pattern, on both
// sides; otherwise it is a star. The parser decides a `**` beside plain
// tokens as it reads, and leaves one beside a brace, or at the edge of a
// branch, to this pass, which runs only then. Such a `**` meets a different
// token in every branch, so the braces are first taken apart by
// `rewrite`; `mayTouch` tells when that can be the case. Then `decide`
// turns every `**` into a globstar or a star by the tokens beside it.

import {
  brace,
  concat,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  N_STAR,
  sep,
  star,
} from "./ast.js";
import { GlobError, MAX_BRACE_NESTING, MAX_EXPANSION } from "../error.js";

// `budget.left` is what distribution may still copy (§6.2).
export function resolveGlobstars(body, budget) {
  if (mayTouch(body)) body = nodeOf(rewrite(body, budget));
  return decide(body, true, true);
}

function hasGlobstar(node) {
  if (node.tag === N_GLOBSTAR) return true;
  if (node.tag === N_CONCAT) return node.children.some(hasGlobstar);
  if (node.tag === N_BRACE) return node.branches.some(hasGlobstar);
  return false;
}

// Is a brace in `node` beside a `**`, or beside another brace when either
// holds a `**`? Only then can distribution change anything.
function mayTouch(node) {
  const seq = node.tag === N_CONCAT ? node.children : [node];
  for (let i = 0; i < seq.length; i++) {
    const item = seq[i];
    if (item.tag !== N_BRACE) continue;
    if (i > 0 && seq[i - 1].tag === N_GLOBSTAR) return true;
    const next = seq[i + 1];
    if (next?.tag === N_GLOBSTAR) return true;
    if (next?.tag === N_BRACE && (hasGlobstar(item) || hasGlobstar(next))) return true;
    if (item.branches.some(mayTouch)) return true;
  }
  return false;
}

// Decides every `**` in `node`, a pattern or a brace branch: a globstar with
// a separator (or the edge of the pattern) on both sides, a star otherwise.
// `before`/`after` tell whether the neighbors of `node` are such boundaries.
// Returns the node that takes its place.
function decide(node, before, after) {
  if (node.tag === N_GLOBSTAR) return before && after ? node : star();
  if (node.tag === N_BRACE) {
    const { branches } = node;
    for (let k = 0; k < branches.length; k++) branches[k] = decide(branches[k], before, after);
    return node;
  }
  if (node.tag !== N_CONCAT) return node;
  const seq = node.children;
  // `**/**` is one `**` (§9.6): a `/`, `**` after a `**` is dropped.
  let kept = 0;
  for (let i = 0; i < seq.length; i++) {
    let item = seq[i];
    if (item.tag === N_GLOBSTAR || item.tag === N_BRACE) {
      const left = kept === 0 ? before : seq[kept - 1].tag === N_SEPARATOR;
      const right = i + 1 === seq.length ? after : seq[i + 1].tag === N_SEPARATOR;
      item = decide(item, left, right);
      if (
        item.tag === N_GLOBSTAR &&
        kept >= 2 &&
        seq[kept - 1].tag === N_SEPARATOR &&
        seq[kept - 2].tag === N_GLOBSTAR
      ) {
        kept--;
        continue;
      }
    }
    // A `*` run is one `*` (§5.2): a `**` turned star next to one.
    if (item.tag === N_STAR && kept > 0 && seq[kept - 1].tag === N_STAR) continue;
    seq[kept++] = item;
  }
  if (kept < seq.length) seq.length = kept;
  return kept === 1 ? seq[0] : node;
}

// ── Distribution ─────────────────────────────────────────────────────────
// Braces taken apart around a `**` (GLOB_SPEC §10.1, §6.2).
//
// A `**` beside a brace meets a different token in every branch, and a `**`
// at the edge of a branch may meet a brace outside. `rewrite` rewrites
// such a sequence until no `**` is left facing a brace edge that may be a
// separator, or empty, in some expansion: `**{/a,b}` becomes `{**/a,**b}`,
// `{**,a}{/,b}` becomes `{**/,**b,a{/,b}}`. Afterwards the tokens beside
// each `**` tell what it meets in every expansion, and `decide` decides it.
//
// The work is linear in the output: every sequence caches the edges of its
// expansions, which no merge changes. What a merge copies is charged to a
// budget (§6.2), and the braces it nests are held to the nesting limit
// (§6.2).

// What an expansion can start or end with, as a bit set.
const SEP = 1; // a separator
const GLOB = 2; // a `**` candidate
const TEXT = 4; // any other token
const NONE = 8; // nothing: the expansion is empty, so the edge lies beyond

const SEP_EDGES = [SEP, SEP];
const GLOB_EDGES = [GLOB, GLOB];
const TEXT_EDGES = [TEXT, TEXT];

// Under distribution a sequence is `{ items, edges, depth }`, and a brace
// holds such sequences with its own edges and depth. `edges` are the
// `[first, last]` edges of the expansions, which no merge changes, so no
// merge has to look inside. `depth` bounds how deep braces nest; it never
// shrinks, so it can run a level or two ahead after a merge flattens a
// brace.
function edges(item) {
  if (item.tag === N_BRACE) return item.edges;
  if (item.tag === N_SEPARATOR) return SEP_EDGES;
  return item.tag === N_GLOBSTAR ? GLOB_EDGES : TEXT_EDGES;
}

// The edges of `a` followed by `b`.
function join(a, b) {
  const through = (edge, beyond) => (edge & NONE ? (edge & ~NONE) | beyond : edge);
  return [through(a[0], b[0]), through(b[1], a[1])];
}

function seqOf(items) {
  const seq = { items, edges: [NONE, NONE], depth: 0 };
  for (const item of items) {
    seq.edges = join(seq.edges, edges(item));
    if (item.tag === N_BRACE) seq.depth = Math.max(seq.depth, item.depth);
  }
  return seq;
}

// A brace of `branches`; a branch that is one brace gives its branches.
function braceOf(branches) {
  const flat = [];
  let first = 0;
  let last = 0;
  let depth = 0;
  for (const branch of branches) {
    const inner = branch.items.length === 1 && branch.items[0].tag === N_BRACE;
    for (const b of inner ? branch.items[0].branches : [branch]) {
      flat.push(b);
      first |= b.edges[0];
      last |= b.edges[1];
      depth = Math.max(depth, b.depth);
    }
  }
  return { tag: N_BRACE, branches: flat, edges: [first, last], depth: depth + 1 };
}

// One sequence for all of `seqs`: the only one, or a brace of them.
function group(seqs) {
  return seqs.length === 1 ? seqs[0] : seqOf([braceOf(seqs)]);
}

function clone(seq) {
  const copy = (item) =>
    item.tag === N_BRACE ? { ...item, branches: item.branches.map(clone) } : item;
  return { ...seq, items: seq.items.map(copy) };
}

// The sequence `node` stands for, with every junction merged.
function rewrite(node, budget) {
  const nodes = node.tag === N_CONCAT ? node.children : [node];
  const out = seqOf([]);
  for (let i = 0; i < nodes.length; i++) {
    let item = nodes[i];
    if (item.tag === N_BRACE) {
      item = braceOf(item.branches.map((branch) => rewrite(branch, budget)));
    } else if (item.tag === N_GLOBSTAR) {
      // Beside what is never a separator, a `**` is a star in every
      // expansion and has nothing to meet.
      const next = i + 1 < nodes.length ? nodes[i + 1] : undefined;
      if (
        (out.edges[1] & (SEP | NONE)) === 0 ||
        (next !== undefined && next.tag !== N_SEPARATOR && next.tag !== N_BRACE)
      ) {
        item = star();
      }
    }
    out.edges = join(out.edges, edges(item));
    push(out, item, budget);
  }
  return out;
}

// Pushes `item` and merges the junction it closes; pushing the merged items
// can close another one with the items before them. Braces may nest no
// deeper than in a pattern (§6.2).
function push(seq, item, budget) {
  if (item.tag === N_BRACE) {
    if (item.depth > MAX_BRACE_NESTING) {
      throw new GlobError("BraceNestingTooDeep", { max: MAX_BRACE_NESTING });
    }
    seq.depth = Math.max(seq.depth, item.depth);
  }
  const { items } = seq;
  items.push(item);
  if (!junction(items)) return;
  const b = items.pop();
  const a = items.pop();
  for (const merged of merge(a, b, budget)) push(seq, merged, budget);
}

// Do the last two items form a junction: a brace or a `**` beside a brace,
// meeting at a `**` edge?
function junction(items) {
  const n = items.length;
  if (n < 2) return false;
  const a = items[n - 2];
  const b = items[n - 1];
  const joins = (item) => item.tag === N_BRACE || item.tag === N_GLOBSTAR;
  return joins(a) && joins(b) && meets(edges(a)[1], edges(b)[0]);
}

// Does a `**` on one edge face a possible separator, or nothing, on the
// other? Otherwise every `**` here is a star in all expansions.
function meets(a, b) {
  const faces = (glob, edge) => (glob & GLOB) !== 0 && (edge & (SEP | NONE)) !== 0;
  return faces(a, b) || faces(b, a);
}

// Merges a junction: one of its braces opens, and each group of its
// branches takes in a copy of the other item.
function merge(a, b, budget) {
  // The first brace opens. The last one does instead after a `**` token, or
  // when every branch of it starts with a `/` token: they share that `/`, so
  // nothing is copied.
  const front =
    a.tag !== N_BRACE ||
    (b.tag === N_BRACE && b.branches.every((br) => br.items[0]?.tag === N_SEPARATOR));
  const { branches } = front ? b : a;
  const other = seqOf([front ? a : b]);
  const groups = split(branches, other.edges[front ? 1 : 0], front);

  if (groups.length > 1) {
    budget.left -= (groups.length - 1) * weight(other.items);
    if (budget.left < 0) throw new GlobError("BraceExpansionTooLarge", { max: MAX_EXPANSION });
  }
  const merged = groups.map((branch, i) =>
    attach(branch, i + 1 < groups.length ? clone(other) : other, front, budget),
  );
  return group(merged).items;
}

// Sorts the branches of an opened brace into groups; each group then takes
// one copy of the other item, whose edge beside them is `facing`. The
// branches that edge does not meet stay one brace. So do the branches with a
// `/` token beside it: they share that one `/`. Every other branch is a group
// of its own.
function split(branches, facing, front) {
  const rest = [];
  const seps = [];
  const groups = [];
  for (const branch of branches) {
    const at = front ? 0 : branch.items.length - 1;
    if (!meets(facing, branch.edges[front ? 0 : 1])) {
      rest.push(branch);
    } else if (branch.items[at]?.tag === N_SEPARATOR) {
      branch.items.splice(at, 1);
      seps.push(seqOf(branch.items));
    } else {
      groups.push(branch);
    }
  }
  if (seps.length > 0) {
    const items = group(seps).items;
    if (front) items.unshift(sep());
    else items.push(sep());
    groups.push(seqOf(items));
  }
  if (rest.length > 0) groups.push(group(rest));
  return groups;
}

// `seq` with `other` behind it, or in front of it, merging the junctions
// that form at the seam.
function attach(seq, other, front, budget) {
  const [head, tail] = front ? [other, seq] : [seq, other];
  head.edges = join(head.edges, tail.edges);
  for (const item of tail.items) push(head, item, budget);
  return head;
}

// What a copy of `items` costs: one per token, a literal or a class by its
// length, and one per branch.
function weight(items) {
  let n = 0;
  for (const item of items) {
    if (item.tag === N_LITERAL) n += item.str.length;
    else if (item.tag === N_CLASS) n += 1 + item.items.length;
    else if (item.tag !== N_BRACE) n += 1;
    else for (const branch of item.branches) n += 1 + weight(branch.items);
  }
  return n;
}

// Back to a node.
function nodeOf(seq) {
  const nodes = seq.items.map((item) =>
    item.tag === N_BRACE ? brace(item.branches.map(nodeOf)) : item,
  );
  return nodes.length === 1 ? nodes[0] : concat(nodes);
}
