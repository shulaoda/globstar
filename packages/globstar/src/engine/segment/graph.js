// AST → the segment graph. One walk keeps a group per distinct (boundary,
// partial segment) reaching a point of the pattern, so a brace adds nodes
// per branch, never a product; past the copy room the segments a brace
// cuts become glued pieces (theory 03, §3).

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
import { WK_AFFIX, NEVER_LEN, compileWild, concatLits, makeWild, GlueNfa } from "./wild.js";
import { LINEAR_CHAINS, keyRoom, segKeysLen, segmentKeys } from "./chain.js";

export const EL_LIT = 0;
export const EL_WILD = 1;
// Zero or more segments, empty ones too.
export const EL_G0 = 2;
// One or more segments: a trailing `/**`.
export const EL_G1 = 3;
// Takes no segment; stands for its successors.
export const EL_JOIN = 4;
// A piece of a segment a brace cuts: its `entry` is its entry state in the
// glue NFA, or INNER when no segment starts in it.
export const EL_PIECE = 5;

export const INNER = -1;

// The successor that ends the pattern.
export const ACCEPT = -1;
// The predecessor of the first nodes.
const START = -2;

// What the last node left behind: the pattern start, a separator or a
// `**/`, or a segment a crossing brace cut (what follows glues to the
// pieces).
const B_START = 0;
const B_SEP = 1;
const B_GLUE = 2;

// Copies a crossing brace may make: this much per unit of pattern weight,
// at least MIN_COPY_ROOM; past it, segments are cut into pieces.
const COPY_PER_WEIGHT = 1;
const MIN_COPY_ROOM = 1 << 14;

// Groups past this many are found by a key of each node's kind (and a
// literal's bytes), and by a key of whole nodes once more than this many
// share one.
const LINEAR_GROUPS = 16;

// Reusable nodes past this many are found by key.
const LINEAR_NODES = 16;

export const isGlobstar = (e) => e.kind === EL_G0 || e.kind === EL_G1;
export const elemNullable = (e) => e.kind === EL_G0 || e.kind === EL_JOIN;

// Can it take some segment? A piece's answer is the glue NFA's.
export const elemSatisfiable = (e) =>
  e.kind === EL_WILD ? e.wild.minLen !== NEVER_LEN : e.kind !== EL_JOIN && e.kind !== EL_PIECE;
export const elemAnySeg = (e) =>
  e.kind === EL_WILD ? e.wild.anySeg : e.kind === EL_G0 || e.kind === EL_G1;

// A copy at its length: V8 grows an array `push` fills by half again and
// never gives the room back.
export function exact(a) {
  return a.slice();
}

function makeElem(kind, litStr, wild) {
  return { kind, litStr: litStr === null ? null : flat(litStr), wild };
}
const G0_ELEM = makeElem(EL_G0, null, null);
const G1_ELEM = makeElem(EL_G1, null, null);
const JOIN_ELEM = makeElem(EL_JOIN, null, null);

// Regroups `body` in place: read it as written first.
export function buildGraph(body, dot, ci) {
  const nodes = seq(body);
  const { linear, weight, paths } = survey(nodes);
  if (!linear) regroup(body);
  const builder = new Builder(dot, ci, linear, weight);
  // Only a union the chain index may file reads its segments' keys.
  if (paths > LINEAR_CHAINS) {
    builder.keys = [];
    builder.keyRoom = keyRoom(weight);
  }
  for (const g of builder.walk(nodes, [firstGroup()])) builder.end(g);
  return builder.finish(weight);
}

// `boundaries`: separators and globstars, inside braces too. `linear`: no
// brace holds a boundary or may leave a `**` beside one. `paths`: an upper
// bound on the paths through the graph.
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
    // A brace that may be empty beside a `**` makes it a globstar in some
    // expansions and a star in others.
    const nextTo = nodes[i + 1]?.tag === N_GLOBSTAR || nodes[i - 1]?.tag === N_GLOBSTAR;
    if (nextTo && nullable(node)) out.linear = false;
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

// A literal weighs its length, a class one plus its ranges, a brace one
// plus its branches (an empty one too), anything else one.
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

// Paths under construction that agree on everything still to come.
// `pendingGlobstar`: a `**/` read but not emitted; a bare `**` right after
// it absorbs it (`x/**/**` ≡ `x/**`). While set, `buf` is empty.
// `doubleStar`: a `**` read at a boundary, a globstar if a `/` or the end
// comes next, a star otherwise. `shadow`: stands for the empty expansions
// of the in-segment braces read since the boundary, which leave a `**`
// beside it (`{a,}**`): lives while the `**` may yet be a globstar, and
// goes on alone once it is.
function makeGroup(tails, state, buf, pendingGlobstar, doubleStar, shadow) {
  return { tails, state, buf, pendingGlobstar, doubleStar, shadow };
}
const firstGroup = () => makeGroup([START], B_START, [], false, false, false);
const forkGroup = (g) =>
  makeGroup(g.tails.slice(), g.state, g.buf.slice(), g.pendingGlobstar, g.doubleStar, g.shadow);

// Inside a segment, where a `**` is a star.
const within = (g) => g.state === B_GLUE || g.buf.length > 0;

function dedupeTails(g) {
  if (g.tails.length < 2) return;
  g.tails.sort((a, b) => a - b);
  let n = 1;
  for (let i = 1; i < g.tails.length; i++) {
    if (g.tails[i] !== g.tails[n - 1]) g.tails[n++] = g.tails[i];
  }
  g.tails.length = n;
}

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

function classEqual(a, b) {
  if (a.neg !== b.neg || a.ranges.length !== b.ranges.length) return false;
  return a.ranges.every((x, i) => x.lo === b.ranges[i].lo && x.hi === b.ranges[i].hi);
}

const agrees = (o, g) =>
  o.state === g.state &&
  o.pendingGlobstar === g.pendingGlobstar &&
  o.doubleStar === g.doubleStar &&
  o.shadow === g.shadow &&
  nodesEqual(o.buf, g.buf);

const flagsKey = (g) =>
  `${g.state},${g.pendingGlobstar ? 1 : 0}${g.doubleStar ? 1 : 0}${g.shadow ? 1 : 0}`;

// Each node by kind, a literal by its bytes too.
function groupKey(g) {
  let key = flagsKey(g);
  for (const n of g.buf) key += n.tag === N_LITERAL ? `|${n.str}` : `|#${n.tag}`;
  return key;
}

function wholeKey(g) {
  let key = flagsKey(g);
  for (const n of g.buf) key += nodeKey(n);
  return key;
}

// A node's content, so that no two share a key: a literal's length first,
// a class its ranges, a brace or sequence its count and then each part.
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

// Groups that agree, merged.
const mergedGroups = () => ({ groups: [], index: null, whole: false });

const keyOf = (merged, g) => (merged.whole ? wholeKey(g) : groupKey(g));

function mergeGroup(merged, g) {
  const n = merged.groups.length;
  let found = -1;
  let refile = false;
  if (n < LINEAR_GROUPS) {
    found = merged.groups.findIndex((o) => agrees(o, g));
  } else {
    if (merged.index === null) indexGroups(merged);
    const key = keyOf(merged, g);
    const bucket = merged.index.get(key);
    found = bucket?.find((i) => agrees(merged.groups[i], g)) ?? -1;
    if (bucket === undefined) merged.index.set(key, [n]);
    else if (found === -1) {
      bucket.push(n);
      refile = !merged.whole && bucket.length > LINEAR_GROUPS;
    }
  }
  if (found === -1) merged.groups.push(g);
  else for (const t of g.tails) merged.groups[found].tails.push(t);
  if (refile) {
    merged.whole = true;
    indexGroups(merged);
  }
}

function indexGroups(merged) {
  merged.index = new Map();
  merged.groups.forEach((o, i) => {
    const key = keyOf(merged, o);
    const bucket = merged.index.get(key);
    if (bucket === undefined) merged.index.set(key, [i]);
    else bucket.push(i);
  });
}

// `at`: a lone predecessor (a number) or the list joined (a string).
const memoKey = (kind, lit, at, glued) => `${kind}${glued ? "+" : ""}:${lit ?? ""}|${at}`;

class Builder {
  constructor(dot, ci, linear, weight) {
    this.elems = [];
    // Per node its successors; empty for a linear pattern, whose nodes
    // follow one another.
    this.succs = [];
    this.start = [];
    this.linear = linear;
    // Literal, globstar and join nodes by (kind, lit, predecessors, glued),
    // for reuse: a scanned list while short, then also an index.
    this.memo = [];
    this.memoIndex = null;
    // Per node, 1 when it goes on the segment of its predecessors; null
    // while no brace cuts a segment.
    this.glued = null;
    // Per node, a wildcard piece's nodes, for the glue NFA.
    this.pieces = [];
    this.room = Math.max(COPY_PER_WEIGHT * weight, MIN_COPY_ROOM);
    this.dot = dot;
    this.ci = ci;
    // A crossing brace's wildcard segments' keys by node, for the chain
    // index; null for any other pattern.
    this.keys = null;
    this.keyRoom = 0;
  }

  finish(weight) {
    let glue = null;
    if (this.glued !== null) {
      const n = this.elems.length;
      const glued = new Uint8Array(n);
      this.glued.forEach((v, i) => (glued[i] = v));
      // A join nothing glues to holds no piece: it only stands for its
      // successors. Successors come later, so one pass back sees each join
      // after them.
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

  // A node for `elem` after `tails`, or the existing one.
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

  // The node made before as (kind, lit, glued) after `tails`, or -1 after
  // recording `id` as it.
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

  // Before a group is copied into `ways` branches, its tails become one
  // join when that keeps fewer edges (`k · ways > k + ways`).
  join(g, ways) {
    if (g.tails.length < 2) return;
    dedupeTails(g);
    const k = g.tails.length;
    if (k * ways <= k + ways) return;
    const id = this.emit(JOIN_ELEM, g.tails, g.state === B_GLUE);
    g.tails.length = 0;
    g.tails.push(id);
  }

  walk(nodes, groups) {
    for (const node of nodes) {
      switch (node.tag) {
        case N_BRACE: {
          if (!crosses(node)) {
            groups = this.push(groups, node);
            break;
          }
          // The open segments go on into every branch, or are cut, and the
          // branches glue to the pieces.
          let held = 0;
          for (const g of groups) for (const n of g.buf) held += nodeWeight(n);
          if (!this.afford((node.branches.length - 1) * held)) groups = this.cutAll(groups);
          for (const g of groups) this.join(g, node.branches.length);
          const merged = mergedGroups();
          for (const branch of node.branches) {
            for (const g of this.walk(seq(branch), groups.map(forkGroup))) mergeGroup(merged, g);
          }
          groups = merged.groups;
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

  // An in-segment node onto every group's partial segment. A brace that
  // may be empty leaves a `**` beside what follows (`{a,}**`): a shadow
  // goes on as if it were empty, and other text ends it.
  push(groups, node) {
    const empty = nullable(node);
    let copies = 0;
    for (const g of groups) if (!g.shadow) copies++;
    if (copies < groups.length && !empty) groups = groups.filter((g) => !g.shadow);
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

  // Before in-segment text: a `**` before it is a star, and a pending
  // `**/` is emitted.
  text(g) {
    this.settle(g);
    if (g.doubleStar) {
      g.doubleStar = false;
      g.buf.push(STAR_NODE);
    }
  }

  // Emit a pending `**/`, always a G0; after a lone G0 it is that one
  // (`**/**/` ≡ `**/`).
  settle(g) {
    if (!g.pendingGlobstar) return;
    const t = g.tails[0];
    const lone = g.tails.length === 1 && t !== START && this.elems[t] === G0_ELEM;
    rebase(g, lone ? t : this.emit(G0_ELEM, g.tails, false), B_SEP);
  }

  // End the partial segment in a piece that what follows glues to.
  cut(g) {
    if (g.buf.length === 0) return;
    rebase(g, this.segment(g, true), B_GLUE);
    g.buf = [];
  }

  cutAll(groups) {
    const merged = mergedGroups();
    for (const g of groups) {
      this.cut(g);
      mergeGroup(merged, g);
    }
    return merged.groups;
  }

  afford(units) {
    if (units > this.room) return false;
    this.room -= units;
    return true;
  }

  // Close every group's segment; they all continue as one group after the
  // separator, and a `**` group takes the `/` as its `**/` and goes on
  // apart, one group per boundary before it.
  closeAll(groups) {
    // The first group closed is the one the others hand their tails to;
    // so is the first `**` group after each boundary.
    let joined = null;
    const starred = [null, null, null];
    for (const g of groups) {
      if (g.shadow && !g.doubleStar) {
        g.tails.length = 0;
        continue;
      }
      let into;
      if (g.doubleStar) {
        // Tails a pending `**/` gathered become one G0, or they pile up
        // (`{**,a}/` × k: k² edges).
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
    const at = out.findIndex((g) => !g.pendingGlobstar);
    if (at > 0) out.unshift(out.splice(at, 1)[0]);
    for (const g of out) dedupeTails(g);
    return out;
  }

  // A bare `**` at the end owns the rest of the path: G0 only from the
  // pattern start, since `x/**` needs a segment after the `/`.
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

  // The node of the group's segment, or of the piece read since the last
  // cut; a wildcard piece keeps its nodes, a wildcard segment of a
  // crossing brace its keys.
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

  close(buf) {
    // An escaped `\` on Windows is a separator no segment holds (GLOB_SPEC
    // §4.2): a wildcard that wants more bytes than any segment has.
    if (buf.some((n) => n.tag === N_LITERAL && hasEscapedSep(n.str))) {
      return makeElem(EL_WILD, null, makeWild(WK_AFFIX, { minLen: NEVER_LEN, variable: false }));
    }
    for (const n of buf) {
      if (n.tag !== N_LITERAL) return makeElem(EL_WILD, null, compileWild(buf, this.dot, this.ci));
    }
    return makeElem(EL_LIT, concatLits(buf, buf.length), null);
  }
}

// Each crossing brace's in-segment branches as one choice, in place:
// `{a,b,/}` walks as `{{a,b},/}`.
function regroup(node) {
  if (node.tag === N_CONCAT) node.children.forEach(regroup);
  if (node.tag !== N_BRACE || !node.branches.some(crosses)) return;
  node.branches.forEach(regroup);
  const at = node.branches.findIndex((b) => !crosses(b));
  if (at === -1) return;
  const within = node.branches.filter((b) => !crosses(b));
  node.branches = node.branches.filter(crosses);
  node.branches.splice(at, 0, within.length === 1 ? within[0] : brace(within));
}

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
