// Which `**` are globstars (GLOB_SPEC §7.0, §8.1).
//
// A pattern means the union of its brace expansions, and in each one a `**`
// is a globstar only with a separator, or the edge of the pattern, on both
// sides; otherwise it is a star. The parser leaves every `**` as it is
// written. Here:
//
// 1. `distribute`, only if `mayTouch`: where a `**` faces a brace edge that
//    may be a separator, or empty, braces are taken apart until the tokens
//    beside each `**` tell what it meets in every expansion.
// 2. `decide`: each `**` becomes a globstar or a star by the tokens beside
//    it.

import {
  N_SEPARATOR,
  N_BRACE,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_CLASS,
  sep,
  star,
  brace,
  concat,
} from "./ast.js";
import { GlobError, MAX_BRACE_NESTING, MAX_EXPANSION } from "./error.js";

// `budget.left` is what step 1 may still copy (§7.7).
export function resolveGlobstars(body, budget) {
  if (!hasGlobstar(body)) return body;
  if (mayTouch(body)) body = nodeOf(distribute(body, budget));
  return decide(body, true, true);
}

function hasGlobstar(node) {
  if (node.tag === N_GLOBSTAR) return true;
  if (node.tag === N_CONCAT) return node.children.some(hasGlobstar);
  if (node.tag === N_BRACE) return node.branches.some(hasGlobstar);
  return false;
}

// The sequence `node` stands for.
function itemsOf(node) {
  return node.tag === N_CONCAT ? node.children : [node];
}

// Is a brace in `node` beside a `**`, or beside another brace (directly, or
// across one `/`) when either holds a `**`? Only then can `distribute` change
// anything.
function mayTouch(node) {
  const seq = itemsOf(node);
  for (let i = 0; i < seq.length; i++) {
    const item = seq[i];
    if (item.tag !== N_BRACE) continue;
    if (i > 0 && seq[i - 1].tag === N_GLOBSTAR) return true;
    let next = seq[i + 1];
    if (next?.tag === N_GLOBSTAR) return true;
    if (next?.tag === N_SEPARATOR) next = seq[i + 2];
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
  // `**/**` is one `**` (§8.6): a `/`, `**` after a `**` is dropped.
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
    seq[kept++] = item;
  }
  if (kept < seq.length) seq.length = kept;
  return kept === 1 ? seq[0] : node;
}

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

// Rewrites `node` until no `**` is left facing a brace edge that may be a
// separator, or empty, in some expansions: `**{/a,b}` becomes `{**/a,**b}`,
// `{**,a}{/,b}` becomes `{**/,**b,a{/,b}}`. Two braces whose `**` edges both
// claim the one `/` between them merge too: lowering can give that `/` to one
// of them only.
function distribute(node, budget) {
  const nodes = itemsOf(node);
  const out = seqOf([]);
  for (let i = 0; i < nodes.length; i++) {
    let item = nodes[i];
    if (item.tag === N_BRACE) {
      item = braceOf(item.branches.map((branch) => distribute(branch, budget)));
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
// deeper than in a pattern (§7.6).
function push(seq, item, budget) {
  if (item.tag === N_BRACE) {
    if (item.depth > MAX_BRACE_NESTING) {
      throw new GlobError("BraceNestingTooDeep", { max: MAX_BRACE_NESTING });
    }
    seq.depth = Math.max(seq.depth, item.depth);
  }
  const { items } = seq;
  items.push(item);
  const width = junction(items);
  if (width === 0) return;
  for (const merged of merge(items.splice(items.length - width), budget)) {
    push(seq, merged, budget);
  }
}

// How many items at the end of `items` form a junction (0: none): two that
// meet, or two braces whose `**` edges both claim the one `/` between them.
function junction(items) {
  const n = items.length;
  if (n < 2) return 0;
  const a = items[n - 2];
  const b = items[n - 1];
  const joins = (item) => item.tag === N_BRACE || item.tag === N_GLOBSTAR;
  if (joins(a) && joins(b) && meets(edges(a)[1], edges(b)[0])) return 2;
  const first = items[n - 3];
  if (
    first?.tag === N_BRACE &&
    a.tag === N_SEPARATOR &&
    b.tag === N_BRACE &&
    first.edges[1] & b.edges[0] & GLOB
  ) {
    return 3;
  }
  return 0;
}

// Does a `**` on one edge face a possible separator, or nothing, on the
// other? Otherwise every `**` here is a star in all expansions.
function meets(a, b) {
  const faces = (glob, edge) => (glob & GLOB) !== 0 && (edge & (SEP | NONE)) !== 0;
  return faces(a, b) || faces(b, a);
}

// Merges a junction: one of its braces opens, and each group of its
// branches takes in a copy of the items on the other side.
function merge(run, budget) {
  // The first brace opens. The last one does instead after a `**` token, or
  // when every branch of it starts with a `/` token: they share that `/`, so
  // nothing is copied.
  const end = run[run.length - 1];
  const front =
    run[0].tag !== N_BRACE ||
    (end.tag === N_BRACE && end.branches.every((b) => b.items[0]?.tag === N_SEPARATOR));
  const { branches } = front ? run.pop() : run.shift();
  const other = seqOf(run);
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
// one copy of the other side, whose edge beside them is `facing`. The
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
    if (item.tag === N_LITERAL) n += item.bytes.length;
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
