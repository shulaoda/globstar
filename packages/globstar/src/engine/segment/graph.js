// AST → the segment graph. JS twin of `crates/globstar/src/engine/
// segment/graph.rs`.
//
// The walk keeps one *group* per distinct (boundary, partial segment)
// reaching a point of the pattern, so a brace whose branches end at a
// `/` costs one node per branch and the continuation is shared;
// nothing is multiplied out.
//
// A segment a separator-crossing brace's edge falls inside (`x{a/,b}y`
// reads the segments `xa` and `xby`) is copied into each branch, and what
// follows the brace onto each segment the branches leave open, while the
// copies weigh no more than the pattern (COPY_PER_WEIGHT). Past that a
// segment is cut into *pieces* at the edge: `x`, `a`, `b` and `y`, the
// pieces after `x` *glued* to it and `y` to `b`, and a piece is never
// copied. So the walk makes every node, edge and token a bounded number
// of times, whatever the braces: the graph is linear in the pattern, and
// every pattern the parser accepts compiles. The glued pieces are matched
// together by one in-segment NFA (`GlueNfa`).
//
// A `**` is a globstar where a boundary is on both sides and a star
// elsewhere, in each brace expansion on its own (GLOB_SPEC §9.1): the walk
// reads it per group, as the expansions reach it.

import {
  brace,
  nullable,
  N_BRACE,
  N_CLASS,
  N_CONCAT,
  N_GLOBSTAR,
  N_LITERAL,
  N_SEPARATOR,
  STAR_NODE,
  seq,
} from "../../parse/ast.js";
import { hasEscapedSep } from "../../bytes.js";
import { flat } from "../../utf8.js";
import { EL_LIT, EL_WILD, EL_G0, EL_G1, EL_JOIN, WK_AFFIX, NEVER_LEN, exact } from "./elem.js";
import { GlueNfa } from "./seg-nfa.js";
import { compileWild, concatLits, makeWild } from "./wild.js";
import { LINEAR_CHAINS, keyRoom, segKeysLen, segmentKeys } from "./chain-index.js";

// Successor marker for "the pattern ends here".
export const ACCEPT = -1;
// Tail marker for "the graph start".
const START = -2;

// What the last node left behind for the next element.
const B_START = 0; // pattern start
const B_SEP = 1; // after a separator or a `**/`
// Inside a segment a crossing brace cut: the tails are its pieces so far,
// and what follows glues to them.
const B_GLUE = 2;

function makeElem(kind, litStr, wild) {
  return { kind, litStr: litStr === null ? null : flat(litStr), wild };
}
// A globstar element holds nothing of its own: every node of a kind
// shares one.
const G0_ELEM = makeElem(EL_G0, null, null);
const G1_ELEM = makeElem(EL_G1, null, null);
const JOIN_ELEM = makeElem(EL_JOIN, null, null);

// Each crossing brace's in-segment branches as one choice, in place:
// `{a,b,/}` walks as `{{a,b},/}`, the choice one piece, not one per
// branch.
function regroup(node) {
  if (node.tag === N_CONCAT) node.children.forEach(regroup);
  if (node.tag !== N_BRACE || !node.branches.some(crosses)) return;
  node.branches.forEach(regroup);
  // The choice takes the first in-segment branch's place.
  const at = node.branches.findIndex((b) => !crosses(b));
  if (at === -1) return;
  const within = node.branches.filter((b) => !crosses(b));
  node.branches = node.branches.filter(crosses);
  node.branches.splice(at, 0, within.length === 1 ? within[0] : brace(within));
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
// and start are implied and never stored. The walk regroups the pattern
// in place (`regroup`), so what needs it as written reads it first.
export function buildGraph(body, dot, ci) {
  const nodes = seq(body);
  const { linear, weight, paths } = survey(nodes);
  // Only a separator-crossing brace can need regrouping.
  if (!linear) regroup(body);
  const b = new Builder(dot, ci, linear, weight);
  // Only a union of more paths than LINEAR_CHAINS indexes them.
  if (paths > LINEAR_CHAINS) {
    b.keys = [];
    b.keyRoom = keyRoom(weight);
  }
  for (const g of b.walk(nodes, [firstGroup()])) b.end(g);
  return b.finish(weight);
}

// What the walk needs to know about the pattern before it starts: its
// separators and globstars, inside braces too (`boundaries`), whether it
// is one path (`linear`: no brace holds a boundary, nor may leave a `**`
// beside one), its `nodeWeight`, and at least as many as the paths
// through the graph (a crossing brace adds its branches', a sequence
// multiplies its nodes').
function survey(nodes) {
  const out = { boundaries: 0, linear: true, weight: 0, paths: 1 };
  for (let i = 0; i < nodes.length; i++) {
    const node = nodes[i];
    if (node.tag === N_SEPARATOR || node.tag === N_GLOBSTAR) {
      out.boundaries++;
      out.weight++;
      continue;
    }
    if (node.tag !== N_BRACE) {
      out.weight += nodeWeight(node);
      continue;
    }
    // A brace that may be empty beside a `**` makes it a globstar in
    // some expansions and a star in others: two paths (`{a,}**`).
    const nextTo = nodes[i + 1]?.tag === N_GLOBSTAR || nodes[i - 1]?.tag === N_GLOBSTAR;
    if (nextTo && nullable(node)) out.linear = false;
    // A brace crosses iff its branches hold a boundary.
    const before = out.boundaries;
    let branchPaths = 0;
    out.weight++;
    for (const b of node.branches) {
      const inner = survey(seq(b));
      out.boundaries += inner.boundaries;
      out.weight += Math.max(1, inner.weight);
      branchPaths = Math.min(branchPaths + inner.paths, Number.MAX_SAFE_INTEGER);
    }
    if (out.boundaries > before) {
      out.linear = false;
      out.paths = Math.min(out.paths * branchPaths, Number.MAX_SAFE_INTEGER);
    }
  }
  return out;
}

// What a node weighs: a literal its length, a class one plus its ranges,
// a brace one plus its branches (an empty one too), anything else one.
// The size bounds that pick a representation scale with it.
export function nodeWeight(node) {
  if (node.tag === N_LITERAL) return node.str.length;
  if (node.tag === N_CLASS) return 1 + node.ranges.length;
  if (node.tag !== N_BRACE) return 1;
  let w = 1;
  for (const b of node.branches) {
    let branch = 0;
    for (const n of seq(b)) branch += nodeWeight(n);
    w += Math.max(1, branch);
  }
  return w;
}

// The room for copies: a segment a crossing brace's edge falls inside is
// copied into its branches, and text after the brace onto each segment
// they leave open (`src/{a/x,b/y}.ts`), while all the copies weigh at
// most this much per unit of pattern weight, never less than
// MIN_COPY_ROOM. Past that the segments are cut into pieces, which copy
// nothing, so the graph stays linear in the pattern. Copies keep a
// pattern's segments whole, as written, and so its chains and literal
// sets; pieces are for braces whose segments multiply.
const COPY_PER_WEIGHT = 1;
const MIN_COPY_ROOM = 1 << 14;

// Paths under construction that agree on everything still to come:
// the nodes awaiting a successor, the boundary just crossed, the
// in-segment nodes read since, and:
// - `pendingGlobstar`: a `**/` read but not yet emitted (a bare `**`
//   right after it absorbs it, `x/**/**` ≡ `x/**`; anything else makes
//   it real; more of them add nothing, `**/**/` ≡ `**/`). While set,
//   `state` is the boundary before it and `buf` is empty.
// - `doubleStar`: a `**` read at a boundary, `buf` empty: a globstar if a
//   `/` or the end of the pattern comes next, a star otherwise (§9.1).
//   The next token decides, in each branch it may be in.
// - `shadow`: the group stands only for the empty expansions of the
//   in-segment braces read since the boundary, which leave a `**` next to
//   it (`{a,}**`, `**{,a}/`). The group that read the braces covers every
//   other reading, the `**` as a star too, so this one lives only while
//   the `**` may yet be a globstar, and goes on as a group of its own
//   once a `/` or the end makes it one.
function makeGroup(tails, state, buf, pendingGlobstar, doubleStar, shadow) {
  return { tails, state, buf, pendingGlobstar, doubleStar, shadow };
}
const firstGroup = () => makeGroup([START], B_START, [], false, false, false);
const forkGroup = (g) =>
  makeGroup(g.tails.slice(), g.state, g.buf.slice(), g.pendingGlobstar, g.doubleStar, g.shadow);
// Inside a segment: a `**` is a star here.
const within = (g) => g.state === B_GLUE || g.buf.length > 0;

// Drop repeated tails, which merges and reused nodes add; their order
// means nothing (successors are listed by node).
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
  if (a.neg !== b.neg || a.ranges.length !== b.ranges.length) return false;
  return a.ranges.every((x, i) => x.lo === b.ranges[i].lo && x.hi === b.ranges[i].hi);
}

// Groups joined when they agree on boundary, pending `**`, and partial
// segment: their continuations are the same nodes. A union leaves a
// group per member, so past LINEAR_GROUPS they are found by a key of
// each node's kind (and a literal's bytes), and by a key of whole nodes
// once more than LINEAR_GROUPS groups apart share one (members apart
// only inside a class or a brace).
const LINEAR_GROUPS = 16;

const agrees = (o, g) =>
  o.state === g.state &&
  o.pendingGlobstar === g.pendingGlobstar &&
  o.doubleStar === g.doubleStar &&
  o.shadow === g.shadow &&
  nodesEqual(o.buf, g.buf);

const flagsKey = (g) =>
  `${g.state},${g.pendingGlobstar ? 1 : 0}${g.doubleStar ? 1 : 0}${g.shadow ? 1 : 0}`;

function groupKey(g) {
  let key = flagsKey(g);
  for (const n of g.buf) key += n.tag === N_LITERAL ? `|${n.str}` : `|#${n.tag}`;
  return key;
}

// The same over all of each node.
function wholeKey(g) {
  let key = flagsKey(g);
  for (const n of g.buf) key += nodeKey(n);
  return key;
}

// A node's content, written so that no two share a key: a literal
// length first, a class its ranges, a brace or sequence its count and
// then each part.
function nodeKey(n) {
  switch (n.tag) {
    case N_LITERAL:
      return `|${n.str.length}:${n.str}`;
    case N_CLASS:
      return `|${n.neg ? "!" : ""}[${n.ranges.map((r) => `${r.lo}-${r.hi}`).join(",")}]`;
    case N_BRACE:
      return `|{${n.branches.length}${n.branches.map(nodeKey).join("")}`;
    case N_CONCAT:
      return `|(${n.children.length}${n.children.map(nodeKey).join("")}`;
    default:
      return `|#${n.tag}`;
  }
}

const keyOf = (out, g) => (out.whole ? wholeKey(g) : groupKey(g));

const merged = () => ({ groups: [], index: null, whole: false });

// Add a group.
function merge(out, g) {
  const n = out.groups.length;
  let found = -1;
  let refile = false;
  if (n < LINEAR_GROUPS) {
    found = out.groups.findIndex((o) => agrees(o, g));
  } else {
    if (out.index === null) indexAll(out);
    const key = keyOf(out, g);
    const bucket = out.index.get(key);
    found = bucket?.find((i) => agrees(out.groups[i], g)) ?? -1;
    if (bucket === undefined) out.index.set(key, [n]);
    else if (found === -1) {
      bucket.push(n);
      refile = !out.whole && bucket.length > LINEAR_GROUPS;
    }
  }
  if (found === -1) out.groups.push(g);
  else for (const t of g.tails) out.groups[found].tails.push(t);
  if (refile) {
    out.whole = true;
    indexAll(out);
  }
}

// File every group by its key.
function indexAll(out) {
  out.index = new Map();
  out.groups.forEach((o, i) => {
    const key = keyOf(out, o);
    const bucket = out.index.get(key);
    if (bucket === undefined) out.index.set(key, [i]);
    else bucket.push(i);
  });
}

// Up to this many reusable nodes are found by a scan, past it by key.
const LINEAR_NODES = 16;
// `at` is a lone predecessor (a number) or the list joined (a string).
const memoKey = (kind, lit, at, glued) => `${kind}${glued ? "+" : ""}:${lit ?? ""}|${at}`;

class Builder {
  constructor(dot, ci, linear, weight) {
    // Elements in emission order and (unless linear) each node's
    // successors in a parallel array.
    this.elems = [];
    this.succs = [];
    this.start = [];
    this.linear = linear;
    // Literal, globstar and join nodes, reused when a branch makes the
    // same (element, predecessors, glued) again: a scanned list while
    // short, then also an index by key.
    this.memo = [];
    this.memoIndex = null;
    // Per node, 1 when it goes on the segment of its predecessors (a
    // group's tails are all glued or none, so this holds for every edge
    // into it); null while no brace cuts a segment.
    this.glued = null;
    // Per node, a wildcard piece's nodes, which `GlueNfa` reads.
    this.pieces = [];
    // What is left of the room for copies (COPY_PER_WEIGHT).
    this.room = Math.max(COPY_PER_WEIGHT * weight, MIN_COPY_ROOM);
    this.dot = dot;
    this.ci = ci;
    // The wildcard segments' keys by node, for a union the chain index
    // may file; null for any other pattern. `keyRoom`: what is left of
    // the key room.
    this.keys = null;
    this.keyRoom = 0;
  }

  // `weight`: the pattern's `nodeWeight`; `keys`: what the chain index
  // files a union's chains by, null when it has none; `glue`: the glued
  // pieces' NFA, null when no brace cuts a segment.
  finish(weight) {
    let glue = null;
    if (this.glued !== null) {
      const n = this.elems.length;
      const glued = new Uint8Array(n);
      this.glued.forEach((v, i) => (glued[i] = v));
      // A join a cut leaves before branches that each start at a `/`, or
      // nothing after a brace, holds no piece: nothing glues to it, so it
      // only stands for its successors, as any join does. A successor's
      // id is past its predecessors', so one pass back sees each join
      // after what follows it.
      for (let i = n - 1; i >= 0; i--) {
        if (
          glued[i] === 1 &&
          this.elems[i].kind === EL_JOIN &&
          !this.succs[i].some((t) => t !== ACCEPT && glued[t] === 1)
        ) {
          glued[i] = 0;
        }
      }
      if (glued.includes(1)) {
        glue = new GlueNfa(this.elems, this.succs, glued, this.pieces, this.ci);
      }
    }
    return {
      elems: exact(this.elems),
      succs: this.succs,
      start: this.start,
      linear: this.linear,
      weight,
      keys: this.keys,
      glue,
    };
  }

  // A node for `elem` after `tails` (`glued` to them or not); an existing
  // one when a branch already made it.
  emit(elem, tails, glued) {
    const id = this.elems.length;
    if (this.linear) {
      this.elems.push(elem);
      return id;
    }
    if (elem.kind !== EL_WILD) {
      const found = this.reuse(elem.kind, elem.litStr, tails, glued, id);
      if (found !== -1) return found;
    }
    this.elems.push(elem);
    this.succs.push([]);
    if (glued) {
      if (this.glued === null) this.glued = [];
      this.glued[id] = 1;
    }
    for (const t of tails) this.link(t, id);
    return id;
  }

  // The node made before as (kind, lit, glued) after `tails`, or -1
  // after recording it as `id`.
  reuse(kind, lit, tails, glued, id) {
    const at = tails.length === 1 ? tails[0] : tails.join(",");
    if (this.memoIndex === null) {
      for (const n of this.memo) {
        if (n.kind === kind && n.lit === lit && n.at === at && n.glued === glued) return n.id;
      }
      this.memo.push({ kind, lit, at, glued, id });
      if (this.memo.length > LINEAR_NODES) {
        this.memoIndex = new Map(
          this.memo.map((n) => [memoKey(n.kind, n.lit, n.at, n.glued), n.id]),
        );
      }
      return -1;
    }
    const key = memoKey(kind, lit, at, glued);
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

  // A group about to be copied into each of `ways` branches: its tails
  // become one join node when that keeps fewer edges, so the branches
  // share an edge to it instead of one from each tail (`{a/,}` × k would
  // give k² edges).
  join(g, ways) {
    if (g.tails.length < 2) return;
    dedupeTails(g);
    const k = g.tails.length;
    if (k * ways <= k + ways) return;
    const id = this.emit(JOIN_ELEM, g.tails, g.state === B_GLUE);
    g.tails.length = 0;
    g.tails.push(id);
  }

  // Walk a sequence of nodes.
  walk(nodes, groups) {
    for (const node of nodes) {
      switch (node.tag) {
        case N_BRACE: {
          if (!crosses(node)) {
            groups = this.push(groups, node);
            break;
          }
          // A segment the brace's edge falls inside goes on into every
          // branch (`x{a/,b}`: `xa` and `xb`), or is cut there and the
          // branches glue to the piece before it.
          let held = 0;
          for (const g of groups) for (const n of g.buf) held += nodeWeight(n);
          if (!this.afford((node.branches.length - 1) * held)) groups = this.cutAll(groups);
          for (const g of groups) this.join(g, node.branches.length);
          const out = merged();
          for (const branch of node.branches) {
            for (const g of this.walk(seq(branch), groups.map(forkGroup))) merge(out, g);
          }
          groups = out.groups;
          for (const g of groups) dedupeTails(g);
          break;
        }
        case N_SEPARATOR:
          groups = this.closeAll(groups);
          break;
        case N_GLOBSTAR:
          if (groups.some((g) => g.shadow && g.doubleStar)) {
            groups = groups.filter((g) => !(g.shadow && g.doubleStar));
          }
          for (const g of groups) {
            if (g.doubleStar || within(g)) {
              // After text, or after a `**`: a star.
              this.text(g);
              g.buf.push(STAR_NODE);
            } else {
              g.doubleStar = true;
            }
          }
          break;
        default:
          groups = this.push(groups, node);
      }
    }
    return groups;
  }

  // Add an in-segment node to every group's partial segment. A brace
  // that may be empty may leave a boundary, or a `**`, next to what
  // follows it (`{a,}**`, `**{,a}/`): a shadow goes on as if it were
  // empty, and other text ends it.
  push(groups, node) {
    const empty = nullable(node);
    let copies = 0;
    for (const g of groups) if (!g.shadow) copies++;
    if (copies < groups.length && !empty) groups = groups.filter((g) => !g.shadow);
    // Text goes on every segment the groups hold, or, past the room for
    // copies, the segments end in pieces and the text is one piece glued
    // to them all (`{a/0,…,a/999}` and a long tail).
    if (copies > 1 && !this.afford((copies - 1) * nodeWeight(node))) {
      groups = this.cutAll(groups);
    }
    let shadows = null;
    for (const g of groups) {
      if (g.shadow) continue;
      if (empty && !within(g)) {
        const s = forkGroup(g);
        s.shadow = true;
        (shadows ??= []).push(s);
      }
      this.text(g);
      g.buf.push(node);
    }
    return shadows === null ? groups : groups.concat(shadows);
  }

  // Ready a group for in-segment text: a `**` before it is a star, and a
  // pending `**/` is real.
  text(g) {
    this.settle(g);
    if (g.doubleStar) {
      g.doubleStar = false;
      g.buf.push(STAR_NODE);
    }
  }

  // Emit a group's pending `**/`. It is always a G0: it absorbs empty
  // segments too, which makes the separators around it lenient. After a
  // lone G0 it is that one (`**/**/` ≡ `**/`).
  settle(g) {
    if (!g.pendingGlobstar) return;
    const t = g.tails[0];
    const lone = g.tails.length === 1 && t !== START && this.elems[t] === G0_ELEM;
    rebase(g, lone ? t : this.emit(G0_ELEM, g.tails, false), B_SEP);
  }

  // End a group's partial segment in a piece: what comes next glues to
  // it.
  cut(g) {
    if (g.buf.length === 0) return;
    rebase(g, this.segment(g, true), B_GLUE);
    g.buf = [];
  }

  // Cut every group (`cut`): those that end in a piece agree again.
  cutAll(groups) {
    const out = merged();
    for (const g of groups) {
      this.cut(g);
      merge(out, g);
    }
    return out.groups;
  }

  // Take `units` of the room for copies, if that much is left.
  afford(units) {
    if (units > this.room) return false;
    this.room -= units;
    return true;
  }

  // Close every group's segment into one node per group; they all
  // continue as one group after the separator. A glued group whose
  // segment ended with its last piece passes its tails on. A `**` group
  // takes the `/` as its `**/`, emitted later, and goes on apart, one
  // group per boundary before it.
  closeAll(groups) {
    // The first group closed is the one they all continue as, and the
    // first `**` group after each boundary the one those continue as:
    // the others hand them their tails. A group left without tails is
    // gone.
    let joined = null;
    const starred = [null, null, null];
    for (const g of groups) {
      if (g.shadow && !g.doubleStar) {
        g.tails.length = 0;
        continue;
      }
      let into;
      if (g.doubleStar) {
        // Tails a pending `**/` gathered become one G0: kept, they would
        // pile up (`{**,a}/` × k: k² edges).
        if (g.tails.length > 1) this.settle(g);
        g.doubleStar = false;
        g.shadow = false;
        g.pendingGlobstar = true;
        into = starred[g.state];
        if (into === null) starred[g.state] = g;
      } else {
        if (g.state !== B_GLUE || g.buf.length > 0) {
          this.settle(g);
          rebase(g, this.segment(g, false), B_SEP);
        }
        g.buf = [];
        g.state = B_SEP;
        into = joined;
        if (into === null) joined = g;
      }
      if (into === null) continue;
      for (const t of g.tails) into.tails.push(t);
      g.tails.length = 0;
    }
    const out = groups.filter((g) => g.tails.length > 0);
    // The joined group first, then the pending `**/`s in order.
    const at = out.findIndex((g) => !g.pendingGlobstar);
    if (at > 0) out.unshift(out.splice(at, 1)[0]);
    for (const g of out) dedupeTails(g);
    return out;
  }

  // End a group at the end of the pattern. A bare `**` owns the rest of
  // the path: pending `**/` add nothing to it, and a separator before
  // them still needs a segment to follow (`x/**/**` ≡ `x/**`), so only
  // the pattern start gives G0.
  end(g) {
    if (g.shadow && !g.doubleStar) return;
    if (g.doubleStar) {
      const elem = g.state === B_START ? G0_ELEM : G1_ELEM;
      this.link(this.emit(elem, g.tails, false), ACCEPT);
    } else if (g.state === B_GLUE && g.buf.length === 0) {
      for (const t of g.tails) this.link(t, ACCEPT);
    } else {
      this.settle(g);
      this.link(this.segment(g, false), ACCEPT);
    }
  }

  // The node of a group's segment, or of the piece of it read since the
  // last cut, after its tails; a piece when glued to them or `cut` (what
  // follows glues to it). A union's wildcard segments keep their keys
  // (its chains are filed by them), and a crossing brace's wildcard
  // pieces their nodes; a wildcard node is never shared, so its id is
  // new.
  segment(g, cut) {
    const elem = this.close(g.buf);
    const wild = elem.kind === EL_WILD;
    const glued = g.state === B_GLUE;
    const id = this.emit(elem, g.tails, glued);
    if (wild && (glued || cut)) this.pieces[id] = g.buf.slice();
    if (wild && this.keys !== null) {
      const keys = segmentKeys(g.buf, this.ci, this.keyRoom);
      if (keys !== null) {
        this.keyRoom -= segKeysLen(keys);
        this.keys[id] = keys;
      }
    }
    return id;
  }

  // The element of a closed segment.
  close(buf) {
    // An escaped `\` on Windows: no segment holds a separator, so a
    // segment with one is a silent miss (§4.2), kept as a wildcard that
    // wants more bytes than any segment has.
    if (buf.some((n) => n.tag === N_LITERAL && hasEscapedSep(n.str))) {
      return makeElem(EL_WILD, null, makeWild(WK_AFFIX, { minLen: NEVER_LEN, variable: false }));
    }
    for (const n of buf) {
      if (n.tag !== N_LITERAL) return makeElem(EL_WILD, null, compileWild(buf, this.dot, this.ci));
    }
    // Empty, one literal, or literals glued across a brace edge.
    return makeElem(EL_LIT, concatLits(buf, buf.length), null);
  }
}
