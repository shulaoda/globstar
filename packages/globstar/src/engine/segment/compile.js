// Ops → segment graph → chains and NFA. JS twin of
// `crates/globstar/src/engine/segment/compile.rs`.
//
// The walk keeps one *group* per distinct (boundary, partial segment)
// reaching a point of the pattern, so a brace whose branches end at a
// `/` costs one node per branch and the continuation is shared;
// nothing is multiplied out.

import {
  OP_LIT,
  OP_ANYCHAR,
  OP_STAR,
  OP_CLASS,
  OP_SEP,
  OP_SEP_RUN,
  OP_OPT_SEGMENTS_SLASH,
  OP_SLASH_ANYTHING,
  OP_GLOBSTAR_ANY,
  OP_LEADING_SEPS,
  OP_ALTERNATION,
  exact,
} from "../ops.js";
import { CI_BYTE } from "../../ast.js";
import { IS_WINDOWS_SEP } from "../../bytes.js";
import { latin1 } from "../../utf8.js";
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
} from "./elem.js";
import { compileSegNfa } from "./seg-nfa.js";
import { GlobError } from "../../error.js";

// Successor marker for "the pattern ends here".
const ACCEPT = -1;
// Tail marker for "the graph start".
const START = -2;

// What the last op left behind for the next element.
const B_START = 0; // pattern start
const B_SEP = 1; // after a separator or a `**/`
const B_OPEN = 2; // a bare `**` absorbs the rest of the path

const EMPTY_BYTES = new Uint8Array(0);

export function compileSegment(ops, dot, ci, forceNfa) {
  const graph = buildGraph(ops, dot, ci);
  let chainList = chains(graph);
  // A lone chain with several globstars is the graph: its NFA serves.
  if (chainList.length === 1 && chainList[0].globstar === MANY_G) chainList = [];
  // A lone chain serves `matchDir` too; anything else needs the NFA
  // (the test hook always does).
  const nfa = forceNfa || chainList.length !== 1 ? anyNfa(graph, ci) : null;
  return { elems: exact(graph.elems), chains: exact(chainList), nfa };
}

function makeElem(kind, litBytes, wild) {
  return { kind, litStr: litBytes !== null ? latin1(litBytes) : null, wild };
}

const isGlobstar = (e) => e.kind === EL_G0 || e.kind === EL_G1;
// Absorbs zero segments too.
const nullable = (e) => e.kind === EL_G0;

// The ops with each crossing brace's in-segment branches as one choice:
// `{a,b,/}` walks as `{{a,b},/}`. Forked one by one, such branches give
// a group per partial segment, 2^k of them after k braces.
function choices(ops) {
  if (!ops.some(splitsChoices)) return ops;
  return ops.map((op) => {
    if (op.kind !== OP_ALTERNATION || !crosses(op)) return op;
    const out = [];
    const within = [];
    let at = -1;
    for (const b of op.branches) {
      if (b.some(crosses)) out.push(choices(b));
      else {
        if (at === -1) at = out.length;
        within.push(b);
      }
    }
    if (at !== -1) {
      out.splice(
        at,
        0,
        within.length === 1 ? within[0] : [{ kind: OP_ALTERNATION, branches: within }],
      );
    }
    return { kind: OP_ALTERNATION, branches: out };
  });
}

// Does a crossing brace in `op` have two or more in-segment branches?
function splitsChoices(op) {
  if (op.kind !== OP_ALTERNATION || !crosses(op)) return false;
  let within = 0;
  for (const b of op.branches) {
    if (!b.some(crosses)) within++;
    else if (b.some(splitsChoices)) return true;
  }
  return within > 1;
}

// Does the op hold a segment boundary?
function crosses(op) {
  switch (op.kind) {
    case OP_LIT:
    case OP_ANYCHAR:
    case OP_STAR:
    case OP_CLASS:
      return false;
    case OP_ALTERNATION:
      for (const b of op.branches) for (const o of b) if (crosses(o)) return true;
      return false;
    default:
      return true;
  }
}

export function opsHaveNonAscii(ops) {
  for (const op of ops) {
    switch (op.kind) {
      case OP_LIT:
        for (let i = 0; i < op.bytes.length; i++) {
          if (op.bytes[i] > 0x7f) return true;
        }
        break;
      case OP_CLASS:
        for (const it of op.cls.items) {
          if (it.tag === CI_BYTE ? it.b > 0x7f : it.hi > 0x7f) return true;
        }
        break;
      case OP_ALTERNATION:
        for (const b of op.branches) {
          if (opsHaveNonAscii(b)) return true;
        }
        break;
      default:
        break;
    }
  }
  return false;
}

// ── the graph ───────────────────────────────────────────────────────

// The graph as the walk leaves it. A pattern without a
// separator-crossing brace is one path, emitted in order: its edges
// and start are implied and never stored.
function buildGraph(ops, dot, ci) {
  let linear = true;
  for (const op of ops) {
    if (op.kind === OP_ALTERNATION && crosses(op)) {
      linear = false;
      break;
    }
  }
  // A linear walk keeps one group and takes a few units per weight, so
  // only a separator-crossing brace can reach the budget.
  let budget = Infinity;
  if (!linear) {
    let weight = 0;
    for (const op of ops) weight += opWeight(op);
    budget = Math.max(MIN_BUDGET, weight * BUDGET_PER_WEIGHT);
  }
  const b = new Builder(dot, ci, linear, budget);
  // Only a separator-crossing brace can need regrouping.
  const groups = b.walk(linear ? ops : choices(ops), [firstGroup()]);
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
  return { elems: b.elems, succs: b.succs, start: b.start, linear };
}

// The work the graph walk may take: 64 units per unit of pattern weight
// (`opWeight`), and never less than 2^20. Patterns of ordinary shape
// need one to three per unit; only a shape whose segments multiply
// (`a{a,/}` repeated, one node per pair of braces) reaches it, and it
// fails before it costs much.
const BUDGET_PER_WEIGHT = 64;
const MIN_BUDGET = 1 << 20;

function tooComplex(max) {
  throw new GlobError("TooComplex", { max });
}

// What an op costs the walk to copy and compile: a literal its length, a
// brace one plus its branches, anything else one.
export function opWeight(op) {
  if (op.kind === OP_LIT) return op.bytes.length;
  return op.kind === OP_ALTERNATION ? braceWeight(op) : 1;
}
function braceWeight(op) {
  let w = 1;
  for (const b of op.branches) for (const o of b) w += opWeight(o);
  return w;
}

// Paths under construction that agree on everything still to come:
// the nodes awaiting a successor, the boundary just crossed, the
// in-segment ops read since, and whether a `**/` was read but not yet
// emitted (a bare `**` right after it absorbs it, `x/**/**` ≡ `x/**`;
// anything else makes it real; more of them add nothing, `**/**/` ≡
// `**/` — while set, `state` is the boundary before it and `buf` is
// empty).
// `weight` is the `opWeight` of `buf`.
function makeGroup(tails, state, buf, pendingOss, weight) {
  return { tails, state, buf, pendingOss, weight };
}
const firstGroup = () => makeGroup([START], B_START, [], false, 0);
const forkGroup = (g) => makeGroup(g.tails.slice(), g.state, g.buf.slice(), g.pendingOss, g.weight);
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

function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function opEqual(a, b) {
  if (a === b) return true;
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case OP_LIT:
      return bytesEqual(a.bytes, b.bytes);
    case OP_CLASS:
      return a.cls === b.cls;
    case OP_ALTERNATION:
      return (
        a.branches.length === b.branches.length &&
        a.branches.every((x, i) => opsEqual(x, b.branches[i]))
      );
    default:
      return true;
  }
}

const opsEqual = (a, b) => a.length === b.length && a.every((op, i) => opEqual(op, b[i]));

// Groups joined when they agree on boundary, pending `**/` and partial
// segment: their continuations are the same nodes. A union leaves a
// group per member, so past LINEAR_GROUPS they are found by a key of
// each op's kind (and a literal's bytes).
const LINEAR_GROUPS = 16;

const agrees = (o, g) =>
  o.state === g.state && o.pendingOss === g.pendingOss && opsEqual(o.buf, g.buf);

function groupKey(g) {
  let key = `${g.state},${g.pendingOss ? 1 : 0}`;
  for (const op of g.buf) key += op.kind === OP_LIT ? `|${latin1(op.bytes)}` : `|#${op.kind}`;
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

// The bytes of the literal ops `ops[from..to]`.
function concatLits(ops, from, to) {
  if (to - from === 1) return ops[from].bytes;
  let len = 0;
  for (let i = from; i < to; i++) len += ops[i].bytes.length;
  const bytes = new Uint8Array(len);
  for (let i = from, at = 0; i < to; at += ops[i].bytes.length, i++) bytes.set(ops[i].bytes, at);
  return bytes;
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

  walk(ops, groups) {
    for (const op of ops) {
      if (this.cost > this.budget) break;
      const k = op.kind;
      // Ops that may still be absorbed by a bare `**` keep the pending
      // `**/` elements; anything else makes them real.
      if (
        k !== OP_GLOBSTAR_ANY &&
        k !== OP_OPT_SEGMENTS_SLASH &&
        k !== OP_LEADING_SEPS &&
        k !== OP_ALTERNATION
      ) {
        for (const g of groups) this.settle(g);
      }
      switch (k) {
        case OP_ALTERNATION: {
          if (!crosses(op)) {
            this.push(groups, op);
            break;
          }
          const out = makeMerged();
          for (const branch of op.branches) {
            for (const g of groups) this.cost += groupCost(g);
            if (this.cost > this.budget) break;
            for (const g of this.walk(branch, groups.map(forkGroup))) merge(out, g);
          }
          groups = out.groups;
          for (const g of groups) dedupeTails(g);
          break;
        }
        case OP_SEP:
        case OP_SEP_RUN:
          this.closeAll(groups, B_SEP);
          break;
        case OP_LEADING_SEPS:
          // `/*`: it only keeps a pending `**/` pending.
          break;
        case OP_OPT_SEGMENTS_SLASH:
          for (const g of groups) if (g.state !== B_OPEN) g.pendingOss = true;
          break;
        case OP_SLASH_ANYTHING:
          this.closeAll(groups, B_SEP);
          this.globstarAll(groups, () => EL_G1);
          break;
        case OP_GLOBSTAR_ANY:
          // The bare `**` owns the rest of the path: pending `**/` add
          // nothing to it, and a separator before them still needs a
          // segment to follow (`x/**/**` ≡ `x/**`), so only the pattern
          // start gives G0.
          for (const g of groups) g.pendingOss = false;
          this.globstarAll(groups, (state) => (state === B_START ? EL_G0 : EL_G1));
          break;
        default:
          this.push(groups, op);
      }
    }
    return groups;
  }

  // Add an in-segment op to every group's partial segment.
  push(groups, op) {
    const weight = opWeight(op);
    for (const g of groups) {
      if (g.state === B_OPEN) continue;
      g.buf.push(op);
      g.weight += weight;
    }
  }

  // Emit a group's pending `**/`. It is always a G0: lowering gives the
  // `/` before a `**/` a lenient `SepRun`, and folds a `**/` into the
  // globstar before it, so none is left behind a strict boundary.
  settle(g) {
    if (!g.pendingOss || g.state === B_OPEN) return;
    rebase(g, this.emit(makeElem(EL_G0, null, null), g.tails), B_SEP);
  }

  // Close every group's segment into one node per group; they all
  // continue as one group at the new boundary.
  closeAll(groups, state) {
    this.joinAll(groups, state, (g) => {
      this.settle(g);
      this.cost += g.weight;
      const buf = g.buf;
      g.buf = [];
      g.weight = 0;
      return this.emit(this.close(buf), g.tails);
    });
  }

  // One globstar node per group, its kind chosen by the boundary the
  // group crossed; they continue as one open group. (The parser never
  // glues a `**` to text, so there is no segment to close.)
  globstarAll(groups, kindOf) {
    this.joinAll(groups, B_OPEN, (g) => this.emit(makeElem(kindOf(g.state), null, null), g.tails));
  }

  // Emit one node per group through `node`, then continue as one group
  // at `state`, in the same list. Open groups pass through untouched.
  joinAll(groups, state, node) {
    let joined = null;
    const open = makeMerged();
    for (const g of groups) {
      if (g.state === B_OPEN) {
        merge(open, g);
        continue;
      }
      const id = node(g);
      if (joined === null) {
        rebase(g, id, state);
        joined = g;
      } else joined.tails.push(id);
    }
    groups.length = 0;
    for (const g of open.groups) groups.push(g);
    if (joined !== null) groups.push(joined);
    for (const g of groups) dedupeTails(g);
  }

  close(buf) {
    // An escaped `\` on Windows: no segment holds a separator, so a
    // segment with one is a silent miss (§2.4), kept as a wildcard that
    // wants more bytes than any segment has.
    if (IS_WINDOWS_SEP && buf.some((op) => op.kind === OP_LIT && op.bytes.includes(0x5c))) {
      return makeElem(EL_WILD, null, makeWild(WK_AFFIX, { minLen: NEVER_LEN, variable: false }));
    }
    for (const op of buf) {
      if (op.kind !== OP_LIT) return makeElem(EL_WILD, null, compileWild(buf, this.dot, this.ci));
    }
    // Empty, one literal, or literals glued across a brace edge.
    const bytes = buf.length === 0 ? EMPTY_BYTES : concatLits(buf, 0, buf.length);
    return makeElem(EL_LIT, bytes, null);
  }
}

// ── chains ──────────────────────────────────────────────────────────

// Every path through the graph as a chain, in pattern order, or none
// when there are more than MAX_CHAINS or a chain with several
// globstars has more than 31 states (the NFA takes those).
function chains(graph) {
  if (graph.linear) {
    const chain = makeChain(graph, null);
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
    const chain = makeChain(graph, nodes);
    if (chain === null) return [];
    out.push(chain);
  }
  return out;
}

// The chain of a path (`null`: the lone path of a brace-free pattern,
// whose nodes are `0..elems.length`), or `null` when its
// several-globstar NFA would not fit an int32.
function makeChain(graph, nodes) {
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

  // Several globstars: the NFA over this path alone; the lone path of
  // a linear pattern is the graph, whose NFA serves.
  let nfa = null;
  if (globstar === MANY_G) {
    if (nodes === null || n + 1 > 31) return null;
    nfa = nfaSmall(graph, nodes);
  }
  // What a path must end with, read off the last element: a cheap
  // reject for the members of a union.
  const last = elem(n - 1);
  let quickSuffixStr = "";
  if (last.kind === EL_LIT) quickSuffixStr = last.litStr;
  else if (last.kind === EL_WILD && last.wild.kind === WK_AFFIX)
    quickSuffixStr = last.wild.suffixStr;
  return { nodes, globstar, nfa, quickSuffixStr };
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

// One int32 of states up to 31, sparse beyond.
function anyNfa(graph, ci) {
  return graph.elems.length + 1 <= 31 ? nfaSmall(graph, null) : nfaSparse(graph, ci);
}

// Int32 active set: at most 31 states (bit 31 is the string-mode bail
// sentinel). The table holds per state the closure entered after
// consuming a segment there (a globstar's includes itself) at `2s` and
// the state's node at `2s + 1`.
function nfaSmall(graph, nodes) {
  const { elems, n, nodeAt, succs, start } = layout(graph, nodes);
  const accept = n;
  const tab = [];
  for (let s = 0; s <= n; s++) tab.push(0, 0);
  let reach1 = 0;
  // The closure a target adds, from the last position back: successors
  // always lie ahead, so each is complete when needed, and a nullable
  // successor contributes its own successors.
  const addTarget = (c, t) => {
    if (t === ACCEPT) return c | (1 << accept);
    if (nullable(elems[nodeAt(t)])) c |= tab[t << 1];
    return c | (1 << t);
  };
  for (let i = n - 1; i >= 0; i--) {
    const el = elems[nodeAt(i)];
    tab[(i << 1) | 1] = nodeAt(i);
    let c = 0;
    if (succs === null) c = addTarget(c, i + 1 < n ? i + 1 : ACCEPT);
    else for (const t of succs[i]) c = addTarget(c, t);
    // A state reaches accept through ≥ 1 segment iff its element can
    // consume one and what follows reaches accept.
    if (elemSatisfiable(el) && ((c & (1 << accept)) !== 0 || (c & reach1) !== 0)) reach1 |= 1 << i;
    // A globstar stays after taking a segment.
    tab[i << 1] = isGlobstar(el) ? c | (1 << i) : c;
  }
  let init = 0;
  for (const t of start) init = addTarget(init, t);
  return { kind: NFA_SMALL, tab: exact(tab), init, accept, reach1 };
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
  const r1 = [];
  const ok = [];
  for (let i = 0; i < n; i++) {
    r1.push(false);
    ok.push(false);
  }
  for (let i = n - 1; i >= 0; i--) {
    let o = false;
    eachSucc(i, (t) => {
      if (t === ACCEPT || r1[t] || (nullable(elems[t]) && ok[t])) o = true;
    });
    ok[i] = o;
    r1[i] = o && elemSatisfiable(elems[i]);
  }
  const reach1 = r1.concat([false]);

  const key = (t) => (ci ? asciiLower(elems[t].litStr) : elems[t].litStr);
  const litAt = [];
  const lits = [];
  const litKeys = [];
  const otherAt = [];
  const others = [];
  const nullAt = [];
  const nulls = [];
  const ends = [];
  const succReach1 = [];
  for (let p = 0; p <= n; p++) {
    litAt.push(lits.length);
    otherAt.push(others.length);
    nullAt.push(nulls.length);
    let end = false;
    let reaches = false;
    const litPos = [];
    eachSucc(p, (t) => {
      if (t === ACCEPT) {
        end = true;
        return;
      }
      if (r1[t]) reaches = true;
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
    ends.push(end);
    succReach1.push(reaches);
  }
  litAt.push(lits.length);
  otherAt.push(others.length);
  nullAt.push(nulls.length);
  return {
    kind: NFA_SPARSE,
    n,
    litAt: exact(litAt),
    lits: exact(lits),
    litKeys: exact(litKeys),
    otherAt: exact(otherAt),
    others: exact(others),
    nullAt: exact(nullAt),
    nulls: exact(nulls),
    ends: exact(ends),
    reach1: exact(reach1),
    succReach1: exact(succReach1),
    accept,
    // Mark arrays reused across calls (see `nfaRunSparse`).
    scratch: null,
  };
}

// ── in-segment matchers ─────────────────────────────────────────────

function makeWild(kind, fields) {
  return {
    kind,
    prefixStr: fields.prefixStr ?? "",
    suffixStr: fields.suffixStr ?? "",
    suffixSetStr: fields.suffixSetStr ?? null,
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

function compileWild(ops, dot, ci) {
  let idx = 0;
  while (idx < ops.length && ops[idx].kind === OP_LIT) idx++;
  const prefix = idx === 0 ? EMPTY_BYTES : concatLits(ops, 0, idx);
  let anychars = 0;
  let hasStar = false;
  while (idx < ops.length) {
    const k = ops[idx].kind;
    if (k === OP_STAR) hasStar = true;
    else if (k === OP_ANYCHAR) anychars += 1;
    else break;
    idx += 1;
  }
  const hasWilds = hasStar || anychars > 0;
  const dotProtect = !dot && prefix.length === 0 && hasWilds;
  // `*` or `?*` with nothing literal around it, up to the suffix.
  const anyHead = prefix.length === 0 && hasStar && anychars <= 1;

  if (idx === ops.length) {
    return makeWild(WK_AFFIX, {
      prefixStr: latin1(prefix),
      suffixStr: "",
      minLen: prefix.length + anychars,
      variable: hasStar,
      dotProtect,
      anychars,
      anySeg: anyHead,
    });
  }
  const suffixes = suffixProduct(ops, idx);
  if (suffixes !== null) {
    const anySeg = anyHead && suffixes.some((s) => s.length === 0);
    if (suffixes.length === 1) {
      return makeWild(WK_AFFIX, {
        prefixStr: latin1(prefix),
        suffixStr: latin1(suffixes[0]),
        minLen: prefix.length + suffixes[0].length + anychars,
        variable: hasStar,
        dotProtect,
        anychars,
        anySeg,
      });
    }
    const common = {
      prefixStr: latin1(prefix),
      minLen: prefix.length + anychars,
      variable: hasStar,
      dotProtect,
      anychars,
      anySeg,
    };
    if (suffixes.length <= LINEAR_SUFFIXES) {
      return makeWild(WK_AFFIX_SET, { ...common, suffixSetStr: suffixes.map(latin1) });
    }
    const { set, runs } = indexSuffixes(suffixes, ci);
    return makeWild(WK_AFFIX_INDEX, { ...common, suffixSetStr: set, runs });
  }
  const nfa = compileSegNfa(ops, dot, ci);
  return makeWild(WK_GENERIC, { dotProtect: !dot && nfa.wildLed, nfa, anySeg: nfa.anySeg });
}

// Literal suffix alternatives one segment enumerates (`*.{a,b,…}`, a
// factored union of names). A product of alternatives (`{a,b}{c,d}…`)
// past this many suffixes stays an NFA, which holds it without
// multiplying it out; a single list is no product and is enumerated
// whatever its length.
const MAX_SUFFIX_PRODUCT = 1024;
// The enumerated suffixes, one byte more each, may not take more than
// this or four times their ops: a long literal next to a product would
// be copied into every suffix.
const MAX_SUFFIX_BYTES = 64 * 1024;

function suffixProduct(ops, from) {
  if (from + 1 === ops.length && ops[from].kind === OP_LIT) {
    return [ops[from].bytes];
  }
  let parts = [[]];
  // Σ (part + 1) bytes, and what the ops themselves hold.
  let size = 1;
  let source = 1;
  const fits = () => size <= Math.max(MAX_SUFFIX_BYTES, source * 4);
  for (let i = from; i < ops.length; i++) {
    const op = ops[i];
    if (op.kind === OP_LIT) {
      size += parts.length * op.bytes.length;
      source += op.bytes.length;
      if (!fits()) return null;
      for (const p of parts) {
        for (let j = 0; j < op.bytes.length; j++) p.push(op.bytes[j]);
      }
    } else if (op.kind === OP_ALTERNATION) {
      const lits = [];
      let branchBytes = 0;
      for (const b of op.branches) {
        if (b.length === 0) lits.push(EMPTY_BYTES);
        else if (b.length === 1 && b[0].kind === OP_LIT) lits.push(b[0].bytes);
        else return null;
        branchBytes += lits[lits.length - 1].length + 1;
      }
      if (parts.length > 1 && parts.length * lits.length > MAX_SUFFIX_PRODUCT) return null;
      // Σ (|p| + |l| + 1) over every pair.
      size = lits.length * (size - parts.length) + parts.length * branchBytes;
      source += branchBytes;
      if (!fits()) return null;
      const next = [];
      for (const p of parts) {
        for (const l of lits) {
          const v = p.slice();
          for (let j = 0; j < l.length; j++) v.push(l[j]);
          next.push(v);
        }
      }
      parts = next;
    } else {
      return null;
    }
  }
  return parts.map((p) => Uint8Array.from(p));
}

// Past LINEAR_SUFFIXES the set is sorted by (length, bytes), ASCII-
// lowercased when case-insensitive, and indexed per length so
// wildConsumes can binary-search it. Latin-1 strings order by byte.
function indexSuffixes(suffixes, ci) {
  let set = suffixes.map(latin1);
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
