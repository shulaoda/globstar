// AST → the segment graph. JS twin of `crates/globstar/src/engine/
// segment/graph.rs`.
//
// The walk keeps one *group* per distinct (boundary, partial segment)
// reaching a point of the pattern, so a brace whose branches end at a
// `/` costs one node per branch and the continuation is shared;
// nothing is multiplied out.

import {
  CI_BYTE,
  brace,
  concat,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  seq,
} from "../../parse/ast.js";
import { IS_WINDOWS_SEP } from "../../bytes.js";
import { flat } from "../../utf8.js";
import { EL_LIT, EL_WILD, EL_G0, EL_G1, WK_AFFIX, WK_GENERIC, NEVER_LEN, exact } from "./elem.js";
import { nfaTableBytes } from "./seg-nfa.js";
import { compileWild, concatLits, makeWild } from "./wild.js";
import { GlobError } from "../../error.js";

// Successor marker for "the pattern ends here".
export const ACCEPT = -1;
// Tail marker for "the graph start".
const START = -2;

// What the last node left behind for the next element.
const B_START = 0; // pattern start
const B_SEP = 1; // after a separator or a `**/`
const B_OPEN = 2; // a bare `**` absorbs the rest of the path

function makeElem(kind, litStr, wild) {
  return { kind, litStr: litStr === null ? null : flat(litStr), wild };
}
// A globstar element holds nothing of its own: every node of a kind
// shares one.
const G0_ELEM = makeElem(EL_G0, null, null);
const G1_ELEM = makeElem(EL_G1, null, null);

// The nodes with each crossing brace's in-segment branches as one
// choice: `{a,b,/}` walks as `{{a,b},/}`. Forked one by one, such
// branches give a group per partial segment, 2^k of them after k braces.
function choices(nodes) {
  return nodes.some(splitsChoices) ? nodes.map(regroup) : nodes;
}

// `node` with its crossing braces' in-segment branches regrouped.
function regroup(node) {
  if (node.tag === N_CONCAT) return concat(choices(node.children));
  if (node.tag !== N_BRACE || !crosses(node)) return node;
  const out = [];
  const within = [];
  let at = -1;
  for (const b of node.branches) {
    if (crosses(b)) out.push(regroup(b));
    else {
      if (at === -1) at = out.length;
      within.push(b);
    }
  }
  if (at !== -1) out.splice(at, 0, within.length === 1 ? within[0] : brace(within));
  return brace(out);
}

// Does a crossing brace in `node` have two or more in-segment branches?
function splitsChoices(node) {
  if (node.tag === N_CONCAT) return node.children.some(splitsChoices);
  if (node.tag !== N_BRACE || !crosses(node)) return false;
  let within = 0;
  for (const b of node.branches) {
    if (!crosses(b)) within++;
    else if (splitsChoices(b)) return true;
  }
  return within > 1;
}

// Does the node hold a segment boundary?
function crosses(node) {
  switch (node.tag) {
    case N_SEPARATOR:
    case N_GLOBSTAR:
      return true;
    case N_BRACE:
      return node.branches.some(crosses);
    case N_CONCAT:
      return node.children.some(crosses);
    default:
      return false;
  }
}

// ── the graph ───────────────────────────────────────────────────────

// The graph as the walk leaves it. A pattern without a
// separator-crossing brace is one path, emitted in order: its edges
// and start are implied and never stored.
export function buildGraph(body, dot, ci) {
  const nodes = seq(body);
  const linear = !nodes.some((n) => n.tag === N_BRACE && crosses(n));
  // A linear walk keeps one group and takes a few units per weight, so
  // only a separator-crossing brace can reach the budget.
  let weight = 0;
  let budget = Infinity;
  if (!linear) {
    for (const n of nodes) weight += nodeWeight(n);
    budget = Math.max(MIN_BUDGET, weight * BUDGET_PER_WEIGHT);
  }
  const b = new Builder(dot, ci, linear, budget);
  // Only a separator-crossing brace can need regrouping.
  const groups = b.walk(linear ? nodes : choices(nodes), false, [firstGroup()]);
  for (const g of groups) {
    b.settle(g);
    if (g.state === B_OPEN) {
      for (const t of g.tails) b.link(t, ACCEPT);
    } else {
      b.cost += g.weight;
      b.link(b.emit(b.close(g.buf), g.tails), ACCEPT);
    }
  }
  if (b.cost > b.budget) tooComplex(b.budget);
  // `weight`: the pattern's `nodeWeight`, read when the literal paths
  // are kept (a linear pattern keeps none, and reads 0).
  return { elems: exact(b.elems), succs: b.succs, start: b.start, linear, weight };
}

// The work the graph walk may take: 64 units per unit of pattern weight
// (`nodeWeight`), and never less than 2^20. A unit is a node, an edge, a
// copied token, or a byte of the tables a wildcard segment builds
// (`tableBytes`). Patterns of ordinary shape need one to three per
// unit; only a shape whose segments multiply (`a{a,/}` repeated, one
// node per pair of braces) reaches it, and it fails before it costs
// much.
const BUDGET_PER_WEIGHT = 64;
const MIN_BUDGET = 1 << 20;

// What the wild's tables take, in the bytes the Rust tables take, so
// both runtimes refuse the same patterns: a set's suffixes and their
// bounds (one more than the suffixes), an NFA's states and kept
// closures. The walk charges it, so a brace pattern whose segments
// multiply cannot keep big tables in each of them (TooComplex).
function tableBytes(w) {
  if (w.kind === WK_AFFIX) return 0;
  if (w.kind === WK_GENERIC) return nfaTableBytes(w.nfa.kinds.length);
  let bytes = 1;
  for (const s of w.suffixSetStr) bytes += s.length + 1;
  return bytes;
}

function tooComplex(max) {
  throw new GlobError("TooComplex", { max });
}

// What a node costs the walk to copy and compile: a literal its length,
// a brace one plus its branches, anything else one.
export function nodeWeight(node) {
  if (node.tag === N_LITERAL) return node.str.length;
  if (node.tag !== N_BRACE) return 1;
  let w = 1;
  for (const b of node.branches) for (const n of seq(b)) w += nodeWeight(n);
  return w;
}

// Paths under construction that agree on everything still to come:
// the nodes awaiting a successor, the boundary just crossed, the
// in-segment nodes read since, and whether a `**/` was read but not yet
// emitted (a bare `**` right after it absorbs it, `x/**/**` ≡ `x/**`;
// anything else makes it real; more of them add nothing, `**/**/` ≡
// `**/` — while set, `state` is the boundary before it and `buf` holds
// at most the in-segment braces read since, `**/{a,b}x`, settled with
// the next node). `tookSep`: a `**` ending a brace branch took the `/` after
// the brace as its `**/`, so that separator closes nothing here.
// `weight` is the `nodeWeight` of `buf`.
function makeGroup(tails, state, buf, pendingGlobstar, tookSep, weight) {
  return { tails, state, buf, pendingGlobstar, tookSep, weight };
}
const firstGroup = () => makeGroup([START], B_START, [], false, false, 0);
const forkGroup = (g) =>
  makeGroup(g.tails.slice(), g.state, g.buf.slice(), g.pendingGlobstar, g.tookSep, g.weight);
// What copying a group costs the walk.
const groupCost = (g) => 1 + g.tails.length + g.weight;

// Drop repeated tails, which merges add; their order means nothing
// (successors are listed by node).
function dedupeTails(g) {
  if (g.tails.length < 2) return;
  g.tails.sort((a, b) => a - b);
  let n = 1;
  for (let i = 1; i < g.tails.length; i++) {
    if (g.tails[i] !== g.tails[n - 1]) g.tails[n++] = g.tails[i];
  }
  g.tails.length = n;
}

// Continue as the one group after `id`, reusing the vectors.
function rebase(g, id, state) {
  g.tails.length = 0;
  g.tails.push(id);
  g.state = state;
  g.pendingGlobstar = false;
}

function nodeEqual(a, b) {
  if (a === b) return true;
  if (a.tag !== b.tag) return false;
  switch (a.tag) {
    case N_LITERAL:
      return a.str === b.str;
    case N_CLASS:
      return classEqual(a, b);
    case N_BRACE:
      return nodesEqual(a.branches, b.branches);
    case N_CONCAT:
      return nodesEqual(a.children, b.children);
    default:
      return true;
  }
}

const nodesEqual = (a, b) => a.length === b.length && a.every((n, i) => nodeEqual(n, b[i]));

// Classes equal by content, as Rust's derived `==` compares them.
function classEqual(a, b) {
  if (a.neg !== b.neg || a.items.length !== b.items.length) return false;
  return a.items.every((x, i) => {
    const y = b.items[i];
    if (x.tag !== y.tag) return false;
    return x.tag === CI_BYTE ? x.b === y.b : x.lo === y.lo && x.hi === y.hi;
  });
}

// Groups joined when they agree on boundary, pending `**/` and partial
// segment: their continuations are the same nodes. A union leaves a
// group per member, so past LINEAR_GROUPS they are found by a key of
// each node's kind (and a literal's bytes).
const LINEAR_GROUPS = 16;

const agrees = (o, g) =>
  o.state === g.state &&
  o.pendingGlobstar === g.pendingGlobstar &&
  o.tookSep === g.tookSep &&
  nodesEqual(o.buf, g.buf);

function groupKey(g) {
  let key = `${g.state},${g.pendingGlobstar ? 1 : 0},${g.tookSep ? 1 : 0}`;
  for (const n of g.buf) key += n.tag === N_LITERAL ? `|${n.str}` : `|#${n.tag}`;
  return key;
}

function makeMerged() {
  return { groups: [], index: null };
}

function merge(out, g) {
  const n = out.groups.length;
  let found = -1;
  if (n < LINEAR_GROUPS) {
    found = out.groups.findIndex((o) => agrees(o, g));
  } else {
    if (out.index === null) {
      out.index = new Map();
      out.groups.forEach((o, i) => {
        const key = groupKey(o);
        const bucket = out.index.get(key);
        if (bucket === undefined) out.index.set(key, [i]);
        else bucket.push(i);
      });
    }
    const key = groupKey(g);
    const bucket = out.index.get(key);
    found = bucket?.find((i) => agrees(out.groups[i], g)) ?? -1;
    if (bucket === undefined) out.index.set(key, [n]);
    else if (found === -1) bucket.push(n);
  }
  if (found === -1) out.groups.push(g);
  else for (const t of g.tails) out.groups[found].tails.push(t);
}

// Up to this many reusable nodes are found by a scan, past it by key.
const LINEAR_NODES = 16;
// `at` is a lone predecessor (a number) or the list joined (a string).
const memoKey = (kind, lit, at) => `${kind}:${lit ?? ""}|${at}`;

class Builder {
  constructor(dot, ci, linear, budget) {
    // Elements in emission order and (unless linear) each node's
    // successors in a parallel array.
    this.elems = [];
    this.succs = [];
    this.start = [];
    this.linear = linear;
    // Literal and globstar nodes, reused when a branch makes the same
    // (element, predecessors) again: a scanned list while short, then
    // also an index by key.
    this.memo = [];
    this.memoIndex = null;
    // The walk's work so far: nodes and their edges, the groups it
    // copied and closed by their cost and weight, and the tables its
    // wildcard segments built. Past `budget` the walk stops and the
    // pattern is too complex.
    this.cost = 0;
    this.budget = budget;
    this.dot = dot;
    this.ci = ci;
  }

  emit(elem, tails) {
    this.cost += 1 + tails.length;
    const id = this.elems.length;
    if (this.linear) {
      this.elems.push(elem);
      return id;
    }
    if (elem.kind !== EL_WILD) {
      const found = this.reuse(elem.kind, elem.litStr, tails, id);
      if (found !== -1) return found;
    }
    this.elems.push(elem);
    this.succs.push([]);
    for (const t of tails) this.link(t, id);
    return id;
  }

  // The node made before as (kind, lit) after `tails`, or -1 after
  // recording it as `id`.
  reuse(kind, lit, tails, id) {
    const at = tails.length === 1 ? tails[0] : tails.join(",");
    if (this.memoIndex === null) {
      for (const n of this.memo) if (n.kind === kind && n.lit === lit && n.at === at) return n.id;
      this.memo.push({ kind, lit, at, id });
      if (this.memo.length > LINEAR_NODES) {
        this.memoIndex = new Map(this.memo.map((n) => [memoKey(n.kind, n.lit, n.at), n.id]));
      }
      return -1;
    }
    const key = memoKey(kind, lit, at);
    const found = this.memoIndex.get(key);
    if (found !== undefined) return found;
    this.memoIndex.set(key, id);
    return -1;
  }

  link(tail, id) {
    if (this.linear) return;
    if (tail === START) this.start.push(id);
    else this.succs[tail].push(id);
  }

  // Walk a sequence of nodes; `sepAfter` tells whether a `/` follows it
  // outside (after the brace it is a branch of).
  walk(nodes, sepAfter, groups) {
    for (let i = 0; i < nodes.length; i++) {
      if (this.cost > this.budget) break;
      const node = nodes[i];
      const sepNext = nodes[i + 1]?.tag === N_SEPARATOR;
      // A `/` follows: next, or after the brace this is a branch of.
      const after = sepNext || (i + 1 === nodes.length && sepAfter);
      // Resolution puts a bare `**` only at the end of the pattern, so no
      // group is open here. Nodes that may still be absorbed by a bare
      // `**` keep the pending `**/` elements; anything else makes them
      // real (a `/` through `closeAll`, which skips the groups it is the
      // `**/` of).
      if (node.tag !== N_GLOBSTAR && node.tag !== N_BRACE && node.tag !== N_SEPARATOR) {
        for (const g of groups) this.settle(g);
      }
      switch (node.tag) {
        case N_BRACE: {
          if (!crosses(node)) {
            this.push(groups, node);
            break;
          }
          // A `**` ending a branch reads the `/` after the brace.
          const out = makeMerged();
          for (const branch of node.branches) {
            for (const g of groups) this.cost += groupCost(g);
            if (this.cost > this.budget) break;
            for (const g of this.walk(seq(branch), after, groups.map(forkGroup))) merge(out, g);
          }
          groups = out.groups;
          for (const g of groups) dedupeTails(g);
          break;
        }
        case N_SEPARATOR:
          this.closeAll(groups);
          break;
        case N_GLOBSTAR:
          if (after) {
            // `**/`: zero or more whole segments, empty ones included, so
            // the separators around it are lenient (§9.5). It is emitted
            // later, when it is known that no bare `**` absorbs it.
            for (const g of groups) {
              g.pendingGlobstar = true;
              g.tookSep = !sepNext;
            }
            if (sepNext) i++;
          } else {
            // A bare `**` owns the rest of the path: pending `**/` add
            // nothing to it (the group continues without them), and a
            // separator before them still needs a segment to follow
            // (`x/**/**` ≡ `x/**`), so only the pattern start gives G0.
            this.globstarAll(groups);
          }
          break;
        default:
          this.push(groups, node);
      }
    }
    return groups;
  }

  // Add an in-segment node to every group's partial segment.
  push(groups, node) {
    const weight = nodeWeight(node);
    for (const g of groups) {
      g.buf.push(node);
      g.weight += weight;
    }
  }

  // Emit a group's pending `**/`. It is always a G0: it absorbs empty
  // segments too, which makes the separators around it lenient.
  settle(g) {
    if (!g.pendingGlobstar) return;
    rebase(g, this.emit(G0_ELEM, g.tails), B_SEP);
  }

  // Close every group's segment into one node per group; they all
  // continue as one group after the separator.
  closeAll(groups) {
    this.joinAll(groups, B_SEP, (g) => {
      this.settle(g);
      this.cost += g.weight;
      const buf = g.buf;
      g.buf = [];
      g.weight = 0;
      return this.emit(this.close(buf), g.tails);
    });
  }

  // One globstar node per group, G0 only at the pattern start; they
  // continue as one open group. (The parser never glues a `**` to text,
  // so there is no segment to close.)
  globstarAll(groups) {
    this.joinAll(groups, B_OPEN, (g) =>
      this.emit(g.state === B_START ? G0_ELEM : G1_ELEM, g.tails),
    );
  }

  // Emit one node per group through `node`, then continue as one group
  // at `state`, in the same list. The branches a `**` ended pass
  // through: they took this `/` already.
  joinAll(groups, state, node) {
    let joined = null;
    const taken = [];
    for (const g of groups) {
      if (g.tookSep) {
        g.tookSep = false;
        taken.push(g);
        continue;
      }
      const id = node(g);
      if (joined === null) {
        rebase(g, id, state);
        joined = g;
      } else joined.tails.push(id);
    }
    groups.length = 0;
    if (joined !== null) groups.push(joined);
    for (const g of taken) groups.push(g);
    for (const g of groups) dedupeTails(g);
  }

  // The element of a closed segment. A wildcard's tables are charged to
  // the walk: a brace pattern whose segments multiply fails early
  // instead of keeping big tables in each of them.
  close(buf) {
    // An escaped `\` on Windows: no segment holds a separator, so a
    // segment with one is a silent miss (§4.2), kept as a wildcard that
    // wants more bytes than any segment has.
    if (IS_WINDOWS_SEP && buf.some((n) => n.tag === N_LITERAL && n.str.includes("\\"))) {
      return makeElem(EL_WILD, null, makeWild(WK_AFFIX, { minLen: NEVER_LEN, variable: false }));
    }
    for (const n of buf) {
      if (n.tag !== N_LITERAL) {
        const wild = compileWild(buf, this.dot, this.ci);
        this.cost += tableBytes(wild);
        return makeElem(EL_WILD, null, wild);
      }
    }
    // Empty, one literal, or literals glued across a brace edge.
    return makeElem(EL_LIT, concatLits(buf, buf.length), null);
  }
}
