// AST → segment graph → chains and NFA. JS twin of
// `crates/globstar/src/engine/segment/compile.rs`.
//
// The walk keeps one *group* per distinct (boundary, partial segment)
// reaching a point of the pattern, so a brace whose branches end at a
// `/` costs one node per branch and the continuation is shared;
// nothing is multiplied out.

import {
  CI_BYTE,
  brace,
  concat,
  N_ANYCHAR,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  N_STAR,
  seq,
} from "../../ast.js";
import { IS_WINDOWS_SEP } from "../../bytes.js";
import { flat } from "../../utf8.js";
import {
  EL_LIT,
  EL_WILD,
  EL_G0,
  EL_G1,
  WK_AFFIX,
  WK_AFFIX_SET,
  WK_AFFIX_INDEX,
  WK_GENERIC,
  MAX_CHAINS,
  LINEAR_SUFFIXES,
  NO_G,
  MANY_G,
  NFA_SMALL,
  NFA_SPARSE,
  elemSatisfiable,
  NEVER_LEN,
  exact,
} from "./elem.js";
import { compileSegNfa } from "./seg-nfa.js";
import { GlobError } from "../../error.js";

// Successor marker for "the pattern ends here".
const ACCEPT = -1;
// Tail marker for "the graph start".
const START = -2;

// What the last node left behind for the next element.
const B_START = 0; // pattern start
const B_SEP = 1; // after a separator or a `**/`
const B_OPEN = 2; // a bare `**` absorbs the rest of the path

const EMPTY = "";

export function compileSegment(body, dot, ci, forceNfa) {
  const graph = buildGraph(body, dot, ci);
  const chainList = chains(graph);
  // A lone chain serves `matchDir` too; anything else needs the NFA
  // (the test hook always does).
  const nfa = forceNfa || chainList.length !== 1 ? anyNfa(graph, ci) : null;
  return { elems: graph.elems, chains: exact(chainList), nfa };
}

function makeElem(kind, litStr, wild) {
  return { kind, litStr: litStr === null ? null : flat(litStr), wild };
}
// A globstar element holds nothing of its own: every node of a kind
// shares one.
const G0_ELEM = makeElem(EL_G0, null, null);
const G1_ELEM = makeElem(EL_G1, null, null);

const isGlobstar = (e) => e.kind === EL_G0 || e.kind === EL_G1;
// Absorbs zero segments too.
const nullable = (e) => e.kind === EL_G0;

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
function buildGraph(body, dot, ci) {
  const nodes = seq(body);
  const linear = !nodes.some((n) => n.tag === N_BRACE && crosses(n));
  // A linear walk keeps one group and takes a few units per weight, so
  // only a separator-crossing brace can reach the budget.
  let budget = Infinity;
  if (!linear) {
    let weight = 0;
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
  return { elems: exact(b.elems), succs: b.succs, start: b.start, linear };
}

// The work the graph walk may take: 64 units per unit of pattern weight
// (`nodeWeight`), and never less than 2^20. Patterns of ordinary shape
// need one to three per unit; only a shape whose segments multiply
// (`a{a,/}` repeated, one node per pair of braces) reaches it, and it
// fails before it costs much.
const BUDGET_PER_WEIGHT = 64;
const MIN_BUDGET = 1 << 20;

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
function makeGroup(tails, state, buf, pendingOss, tookSep, weight) {
  return { tails, state, buf, pendingOss, tookSep, weight };
}
const firstGroup = () => makeGroup([START], B_START, [], false, false, 0);
const forkGroup = (g) =>
  makeGroup(g.tails.slice(), g.state, g.buf.slice(), g.pendingOss, g.tookSep, g.weight);
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
  g.pendingOss = false;
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
  o.pendingOss === g.pendingOss &&
  o.tookSep === g.tookSep &&
  nodesEqual(o.buf, g.buf);

function groupKey(g) {
  let key = `${g.state},${g.pendingOss ? 1 : 0},${g.tookSep ? 1 : 0}`;
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
    if (bucket !== undefined) found = bucket.find((i) => agrees(out.groups[i], g)) ?? -1;
    if (found === -1) {
      if (bucket === undefined) out.index.set(key, [n]);
      else bucket.push(n);
    }
  }
  if (found === -1) out.groups.push(g);
  else for (const t of g.tails) out.groups[found].tails.push(t);
}

// Up to this many reusable nodes are found by a scan, past it by key.
const LINEAR_NODES = 16;
// `at` is a lone predecessor (a number) or the list joined (a string).
const memoKey = (kind, lit, at) => `${kind}:${lit ?? ""}|${at}`;

// The bytes of the literal nodes `nodes[from..to]`.
function concatLits(nodes, from, to) {
  let s = "";
  for (let i = from; i < to; i++) s += nodes[i].str;
  return s;
}

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
    // The walk's work so far: nodes and their edges, and the groups it
    // copied and closed by their cost and weight. Past `budget` the walk
    // stops and the pattern is too complex.
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
      const last = i + 1 === nodes.length;
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
          const after = sepNext || (last && sepAfter);
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
          if (sepNext || (last && sepAfter)) {
            // `**/`: zero or more whole segments, empty ones included, so
            // the separators around it are lenient (§12.3). It is emitted
            // later, when it is known that no bare `**` absorbs it.
            for (const g of groups) {
              g.pendingOss = true;
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
    if (!g.pendingOss) return;
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

  close(buf) {
    // An escaped `\` on Windows: no segment holds a separator, so a
    // segment with one is a silent miss (§2.4), kept as a wildcard that
    // wants more bytes than any segment has.
    if (IS_WINDOWS_SEP && buf.some((n) => n.tag === N_LITERAL && n.str.includes("\\"))) {
      return makeElem(EL_WILD, null, makeWild(WK_AFFIX, { minLen: NEVER_LEN, variable: false }));
    }
    for (const n of buf) {
      if (n.tag !== N_LITERAL) return makeElem(EL_WILD, null, compileWild(buf, this.dot, this.ci));
    }
    // Empty, one literal, or literals glued across a brace edge.
    return makeElem(EL_LIT, concatLits(buf, 0, buf.length), null);
  }
}

// ── chains ──────────────────────────────────────────────────────────

// Every path through the graph as a chain, in pattern order, or none
// when there are more than MAX_CHAINS or a chain with several
// globstars has more than 32 states (the NFA takes those).
function chains(graph) {
  if (graph.linear) {
    const chain = makeChain(graph, null, true);
    return chain === null ? [] : [chain];
  }
  // Depth-first, first branch first, along one path: a level per node
  // on it (the start's below them) and the successor it tries next. A
  // path is copied only when it reaches the end.
  const paths = [];
  const path = [];
  const succs = [graph.start];
  const next = [0];
  while (succs.length > 0) {
    const top = succs.length - 1;
    if (next[top] === succs[top].length) {
      succs.pop();
      next.pop();
      path.pop();
      continue;
    }
    const id = succs[top][next[top]++];
    if (id === ACCEPT) {
      paths.push(path.slice());
      if (paths.length > MAX_CHAINS) return [];
      continue;
    }
    path.push(id);
    succs.push(graph.succs[id]);
    next.push(0);
  }
  const out = [];
  for (const nodes of paths) {
    const chain = makeChain(graph, nodes, paths.length === 1);
    if (chain === null) return [];
    out.push(chain);
  }
  return out;
}

// The chain of a path (`null`: the lone path of a brace-free pattern,
// whose nodes are `0..elems.length`), or `null` when it has several
// globstars and is the graph's `lone` path (the graph's NFA serves) or
// its NFA would not fit an int32.
function makeChain(graph, nodes, lone) {
  const elems = graph.elems;
  const n = nodes === null ? elems.length : nodes.length;
  const elem = (i) => elems[nodes === null ? i : nodes[i]];
  let globstar = NO_G;
  for (let i = 0; i < n; i++) {
    if (!isGlobstar(elem(i))) continue;
    if (globstar !== NO_G) {
      globstar = MANY_G;
      break;
    }
    globstar = i;
  }

  // Several globstars: the NFA over this path alone.
  let nfa = null;
  if (globstar === MANY_G) {
    if (lone || n + 1 > 32) return null;
    nfa = nfaSmall(graph, nodes);
  }
  // What a path must end with, read off the last element: a cheap
  // reject for the members of a union.
  const last = elem(n - 1);
  let quickSuffixStr = "";
  if (last.kind === EL_LIT) quickSuffixStr = last.litStr;
  else if (last.kind === EL_WILD && last.wild.kind === WK_AFFIX)
    quickSuffixStr = last.wild.suffixStr;
  // A chain's own NFA keeps the chain's elements: no node list then.
  return { nodes: nfa === null ? nodes : null, globstar, nfa, quickSuffixStr };
}

// ── the NFA ─────────────────────────────────────────────────────────

// The NFA over the positions of a path (`nodes === null`: the graph
// itself). Positions of a path follow one another; only the graph of
// a brace pattern has stored edges. States are numbered by position;
// the accept state comes last.
function layout(graph, nodes) {
  const elems = graph.elems;
  const n = nodes === null ? elems.length : nodes.length;
  const chained = nodes !== null || graph.linear;
  return {
    elems,
    n,
    nodeAt: (i) => (nodes === null ? i : nodes[i]),
    succs: chained ? null : graph.succs,
    start: chained ? [0] : graph.start,
  };
}

// One int32 of states up to 32, sparse beyond.
function anyNfa(graph, ci) {
  return graph.elems.length + 1 <= 32 ? nfaSmall(graph, null) : nfaSparse(graph, ci);
}

// Int32 active set: at most 32 states. `after` holds per state the
// closure entered after consuming a segment there (a globstar's
// includes itself); state `i` takes what `elems[i]` takes: the graph's
// elements, or a chain's in path order.
function nfaSmall(graph, nodes) {
  const { elems, n, nodeAt, succs, start } = layout(graph, nodes);
  const accept = n;
  const after = [];
  for (let s = 0; s <= n; s++) after.push(0);
  let reach1 = 0;
  // The closure a target adds, from the last position back: successors
  // always lie ahead, so each is complete when needed, and a nullable
  // successor contributes its own successors.
  const addTarget = (c, t) => {
    if (t === ACCEPT) return c | (1 << accept);
    if (nullable(elems[nodeAt(t)])) c |= after[t];
    return c | (1 << t);
  };
  for (let i = n - 1; i >= 0; i--) {
    const el = elems[nodeAt(i)];
    let c = 0;
    if (succs === null) c = addTarget(c, i + 1 < n ? i + 1 : ACCEPT);
    else for (const t of succs[i]) c = addTarget(c, t);
    // A state reaches accept through ≥ 1 segment iff its element can
    // consume one and what follows reaches accept.
    if (elemSatisfiable(el) && ((c & (1 << accept)) !== 0 || (c & reach1) !== 0)) reach1 |= 1 << i;
    // A globstar stays after taking a segment.
    after[i] = isGlobstar(el) ? c | (1 << i) : c;
  }
  let init = 0;
  for (const t of start) init = addTarget(init, t);
  const own = nodes === null ? elems : nodes.map((i) => elems[i]);
  return { kind: NFA_SMALL, elems: own, after: exact(after), init, accept, reach1 };
}

// Past 31 states the graph NFA is stepped sparsely. A position keeps
// its successors, not a closure row (the start is the last position):
// the literal ones' entry states sorted by literal (`litKeys` folded
// when insensitive), the other positions, and the nullable ones (`**/`)
// among those, at `xs[xAt[p]..xAt[p + 1]]`. The active set holds states
// and whole positions; see `nfaRunSparse`.
function nfaSparse(graph, ci) {
  const { elems, n, succs, start } = layout(graph, null);
  const succOf = (p) => (p === n ? start : succs === null ? null : succs[p]);
  // Linear graphs store no edges: position `p` is followed by `p + 1`.
  const eachSucc = (p, f) => {
    const list = succOf(p);
    if (list === null) f(p + 1 < n ? p + 1 : ACCEPT);
    else for (const t of list) f(t);
  };
  // State `p` is position `p`; the accept state is `n`.
  const accept = n;

  // A position reaches accept through ≥ 1 segment iff its element can
  // consume one and what it enters reaches accept: from the last
  // position back, as successors always lie ahead.
  // The flags are bytes (1 or 0) and the offsets int32: a plain array
  // takes 8 bytes per entry, booleans included.
  // (A nullable successor is a `**/`, which reaches accept through one
  // segment whenever it reaches it at all.)
  const reach1 = new Uint8Array(n + 1);
  for (let i = n - 1; i >= 0; i--) {
    let o = 0;
    eachSucc(i, (t) => {
      if (t === ACCEPT || reach1[t]) o = 1;
    });
    reach1[i] = o && elemSatisfiable(elems[i]) ? 1 : 0;
  }

  const key = (t) => (ci ? asciiLower(elems[t].litStr) : elems[t].litStr);
  const litAt = new Int32Array(n + 2);
  const otherAt = new Int32Array(n + 2);
  const nullAt = new Int32Array(n + 2);
  const ends = new Uint8Array(n + 1);
  const succReach1 = new Uint8Array(n + 1);
  const lits = [];
  const litKeys = [];
  const others = [];
  const nulls = [];
  for (let p = 0; p <= n; p++) {
    litAt[p] = lits.length;
    otherAt[p] = others.length;
    nullAt[p] = nulls.length;
    const litPos = [];
    eachSucc(p, (t) => {
      if (t === ACCEPT) {
        ends[p] = 1;
        return;
      }
      if (reach1[t]) succReach1[p] = 1;
      if (elems[t].kind === EL_LIT) litPos.push(t);
      else {
        others.push(t);
        if (nullable(elems[t])) nulls.push(t);
      }
    });
    const keys = litPos.map(key);
    const order = litPos.map((_, i) => i);
    order.sort((a, b) => (keys[a] < keys[b] ? -1 : keys[a] > keys[b] ? 1 : litPos[a] - litPos[b]));
    for (const i of order) {
      lits.push(litPos[i]);
      litKeys.push(keys[i]);
    }
  }
  litAt[n + 1] = lits.length;
  otherAt[n + 1] = others.length;
  nullAt[n + 1] = nulls.length;
  return {
    kind: NFA_SPARSE,
    n,
    litAt,
    lits: Int32Array.from(lits),
    litKeys: exact(litKeys),
    otherAt,
    others: Int32Array.from(others),
    nullAt,
    nulls: Int32Array.from(nulls),
    ends,
    reach1,
    succReach1,
    accept,
    // Mark arrays reused across calls (see `nfaRunSparse`).
    scratch: null,
  };
}

// ── in-segment matchers ─────────────────────────────────────────────

function makeWild(kind, fields) {
  return {
    kind,
    prefixStr: flat(fields.prefixStr ?? ""),
    suffixStr: flat(fields.suffixStr ?? ""),
    suffixSetStr: fields.suffixSetStr?.map(flat) ?? null,
    runs: fields.runs ?? null,
    minLen: fields.minLen ?? 0,
    variable: fields.variable ?? true,
    dotProtect: fields.dotProtect ?? false,
    anychars: fields.anychars ?? 0,
    nfa: fields.nfa ?? null,
    // Its wildcards alone match every non-empty segment (`*`, `?*`,
    // `{a,*}`); drives `matchDir`'s all-below bit.
    anySeg: fields.anySeg ?? false,
  };
}

function compileWild(nodes, dot, ci) {
  let idx = 0;
  while (idx < nodes.length && nodes[idx].tag === N_LITERAL) idx++;
  const prefix = concatLits(nodes, 0, idx);
  let anychars = 0;
  let hasStar = false;
  while (idx < nodes.length) {
    const t = nodes[idx].tag;
    if (t === N_STAR) hasStar = true;
    else if (t === N_ANYCHAR) anychars += 1;
    else break;
    idx += 1;
  }
  // A literal product after the wildcards (one empty suffix when nothing
  // follows them) keeps the segment anchored at both ends; anything else
  // runs the NFA.
  const suffixes = suffixProduct(nodes, idx);
  if (suffixes === null) {
    const nfa = compileSegNfa(nodes, dot, ci);
    return makeWild(WK_GENERIC, { dotProtect: !dot && nfa.wildLed, nfa, anySeg: nfa.anySeg });
  }
  const fields = {
    prefixStr: prefix,
    minLen: prefix.length + anychars,
    variable: hasStar,
    dotProtect: !dot && prefix.length === 0 && (hasStar || anychars > 0),
    anychars,
    // `*` or `?*` with nothing literal around it, up to a suffix that
    // may be empty.
    anySeg: prefix.length === 0 && hasStar && anychars <= 1 && suffixes.some((s) => s.length === 0),
  };
  if (suffixes.length === 1) {
    fields.suffixStr = suffixes[0];
    fields.minLen += suffixes[0].length;
    return makeWild(WK_AFFIX, fields);
  }
  if (suffixes.length <= LINEAR_SUFFIXES) {
    fields.suffixSetStr = suffixes;
    return makeWild(WK_AFFIX_SET, fields);
  }
  const { set, runs } = indexSuffixes(suffixes, ci);
  fields.suffixSetStr = set;
  fields.runs = runs;
  return makeWild(WK_AFFIX_INDEX, fields);
}

// Literal suffix alternatives one segment enumerates (`*.{a,b,…}`, a
// factored union of names). A product of alternatives (`{a,b}{c,d}…`)
// past this many suffixes stays an NFA, which holds it without
// multiplying it out; a single list is no product and is enumerated
// whatever its length.
const MAX_SUFFIX_PRODUCT = 1024;
// The enumerated suffixes, one byte more each, may not take more than
// this or four times their nodes: a long literal next to a product
// would be copied into every suffix.
const MAX_SUFFIX_BYTES = 64 * 1024;

function suffixProduct(nodes, from) {
  if (from + 1 === nodes.length && nodes[from].tag === N_LITERAL) {
    return [nodes[from].str];
  }
  let parts = [""];
  // Σ (part + 1) bytes, and what the nodes themselves hold.
  let size = 1;
  let source = 1;
  const fits = () => size <= Math.max(MAX_SUFFIX_BYTES, source * 4);
  for (let i = from; i < nodes.length; i++) {
    const node = nodes[i];
    if (node.tag === N_LITERAL) {
      size += parts.length * node.str.length;
      source += node.str.length;
      if (!fits()) return null;
      parts = parts.map((p) => p + node.str);
    } else if (node.tag === N_BRACE) {
      const lits = [];
      let branchBytes = 0;
      for (const branch of node.branches) {
        const b = seq(branch);
        if (b.length === 0) lits.push(EMPTY);
        else if (b.length === 1 && b[0].tag === N_LITERAL) lits.push(b[0].str);
        else return null;
        branchBytes += lits[lits.length - 1].length + 1;
      }
      if (parts.length > 1 && parts.length * lits.length > MAX_SUFFIX_PRODUCT) return null;
      // Σ (|p| + |l| + 1) over every pair.
      size = lits.length * (size - parts.length) + parts.length * branchBytes;
      source += branchBytes;
      if (!fits()) return null;
      const next = [];
      for (const p of parts) for (const l of lits) next.push(p + l);
      parts = next;
    } else {
      return null;
    }
  }
  return parts;
}

// Past LINEAR_SUFFIXES the set is sorted by (length, bytes), ASCII-
// lowercased when case-insensitive, and indexed per length so
// wildConsumes can binary-search it. Latin-1 strings order by byte.
function indexSuffixes(suffixes, ci) {
  let set = suffixes;
  if (ci) set = set.map(asciiLower);
  set.sort((a, b) => a.length - b.length || (a < b ? -1 : a > b ? 1 : 0));
  set = set.filter((s, i) => i === 0 || s !== set[i - 1]);
  const runs = [];
  for (let i = 0; i < set.length; i++) {
    const len = set[i].length;
    const last = runs.length > 0 ? runs[runs.length - 1] : null;
    if (last !== null && last.len === len) last.end = i + 1;
    else runs.push({ len, start: i, end: i + 1 });
  }
  return { set: exact(set), runs: exact(runs) };
}

function asciiLower(s) {
  let out = "";
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    out += c >= 0x41 && c <= 0x5a ? String.fromCharCode(c + 32) : s[i];
  }
  return out;
}
