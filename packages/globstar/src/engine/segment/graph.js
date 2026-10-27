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
import { hasEscapedSep } from "../bytes.js";
import { WK_AFFIX, NEVER_LEN, compileWild, concatLits, makeWild, GlueNfa } from "./wild.js";

export const EL_LIT = 0;
export const EL_WILD = 1;
export const EL_G0 = 2;
export const EL_G1 = 3;
export const EL_JOIN = 4;
export const EL_PIECE = 5;

export const INNER = -1;

export const ACCEPT = -1;
const START = -2;

const B_START = 0;
const B_SEP = 1;
const B_GLUE = 2;

// What one kind of expansion may take for a pattern: four units per unit
// of its weight, at least 64 KiB. Each kind draws from a room of its own,
// and past it keeps the pattern's form unexpanded.
export const room = (weight) => Math.max(4 * weight, 1 << 16);

// A copied token is compiled again in every copy: a copy costs this many
// units of the room per unit of its weight.
const COPY_COST = 4;

const LINEAR_GROUPS = 16;

export const isGlobstar = (e) => e.kind === EL_G0 || e.kind === EL_G1;
export const elemNullable = (e) => e.kind === EL_G0 || e.kind === EL_JOIN;

export const elemSatisfiable = (e) =>
  e.kind === EL_WILD ? e.wild.minLen !== NEVER_LEN : e.kind !== EL_JOIN && e.kind !== EL_PIECE;
export const elemAnySeg = (e) =>
  e.kind === EL_WILD ? e.wild.anySeg : e.kind === EL_G0 || e.kind === EL_G1;

export function exact(a) {
  return a.slice();
}

function makeElem(kind, litStr, wild) {
  return { kind, litStr, wild };
}
const G0_ELEM = makeElem(EL_G0, null, null);
const G1_ELEM = makeElem(EL_G1, null, null);
const JOIN_ELEM = makeElem(EL_JOIN, null, null);

export function buildGraph(body, dot, ci) {
  const union = body.tag === N_BRACE && body.branches.length > 1;
  const { linear: plain, weight } = survey(seq(body));
  const linear = plain && !union;
  if (!linear) {
    if (union) body.branches.forEach(regroup);
    else regroup(body);
  }
  const builder = new Builder(dot, ci, linear, weight);
  const groups = union
    ? builder.choice(body.branches, [firstGroup()])
    : builder.walk(seq(body), [firstGroup()]);
  for (const g of groups) builder.end(g);
  return builder.finish(weight);
}

function survey(nodes) {
  const out = { boundaries: 0, linear: true, weight: 0 };
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
    const nextTo = nodes[i + 1]?.tag === N_GLOBSTAR || nodes[i - 1]?.tag === N_GLOBSTAR;
    if (nextTo && nullable(node)) out.linear = false;
    const before = out.boundaries;
    out.weight++;
    for (const b of node.branches) {
      const inner = survey(seq(b));
      out.boundaries += inner.boundaries;
      out.weight += Math.max(1, inner.weight);
    }
    if (out.boundaries > before) out.linear = false;
  }
  return out;
}

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

function makeGroup(tails, state, buf, pendingGlobstar, doubleStar, shadow) {
  return { tails, state, buf, pendingGlobstar, doubleStar, shadow };
}
const firstGroup = () => makeGroup([START], B_START, [], false, false, false);
const forkGroup = (g) =>
  makeGroup(g.tails.slice(), g.state, g.buf.slice(), g.pendingGlobstar, g.doubleStar, g.shadow);

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

function memoKey(kind, lit, tails, glued) {
  let h = Math.imul(0x811c9dc5 ^ (kind * 2 + (glued ? 1 : 0)), 0x01000193);
  if (lit !== null) {
    h = Math.imul(h ^ lit.length, 0x01000193);
    const n = Math.min(lit.length, 64);
    for (let i = 0; i < n; i++) h = Math.imul(h ^ lit.charCodeAt(i), 0x01000193);
  }
  h = Math.imul(h ^ tails.length, 0x01000193);
  for (let i = 0; i < tails.length; i++) h = Math.imul(h ^ tails[i], 0x01000193);
  return h >>> 2;
}

function sameTails(at, tails) {
  if (typeof at === "number") return tails.length === 1 && tails[0] === at;
  if (at.length !== tails.length) return false;
  for (let i = 0; i < at.length; i++) if (at[i] !== tails[i]) return false;
  return true;
}

class Builder {
  constructor(dot, ci, linear, weight) {
    this.elems = [];
    this.succs = [];
    this.start = [];
    this.linear = linear;
    this.memo = linear ? null : new Map();
    this.glued = null;
    this.pieces = [];
    this.room = room(weight);
    this.suffixPool = room(weight);
    this.dot = dot;
    this.ci = ci;
  }

  finish(weight) {
    let glue = null;
    if (this.glued !== null) {
      const n = this.elems.length;
      const glued = new Uint8Array(n);
      this.glued.forEach((v, i) => (glued[i] = v));
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
      glue,
    };
  }

  emit(elem, tails, glued) {
    if (this.linear) {
      this.elems.push(elem);
      return this.elems.length - 1;
    }
    const key = elem.kind === EL_WILD ? -1 : memoKey(elem.kind, elem.litStr, tails, glued);
    const found = key === -1 ? undefined : this.memo.get(key);
    if (
      found !== undefined &&
      found.kind === elem.kind &&
      found.lit === elem.litStr &&
      found.glued === glued &&
      sameTails(found.at, tails)
    ) {
      return found.id;
    }
    const id = this.elems.length;
    this.elems.push(elem);
    this.succs.push([]);
    if (glued) {
      if (this.glued === null) this.glued = [];
      this.glued[id] = 1;
    }
    for (const t of tails) this.link(t, id);
    if (key !== -1 && found === undefined) {
      const at = tails.length === 1 ? tails[0] : tails.slice();
      this.memo.set(key, { kind: elem.kind, lit: elem.litStr, at, glued, id });
    }
    return id;
  }

  link(tail, id) {
    if (this.linear) return;
    if (tail === START) this.start.push(id);
    else this.succs[tail].push(id);
  }

  // Several tails pass through one join, before a node follows them or a
  // brace copies them: every other node has one predecessor.
  gather(g) {
    if (g.tails.length < 2) return;
    dedupeTails(g);
    if (g.tails.length < 2) return;
    const id = this.emit(JOIN_ELEM, g.tails, g.state === B_GLUE);
    g.tails.length = 0;
    g.tails.push(id);
  }

  choice(branches, groups) {
    let held = 0;
    for (const g of groups) for (const n of g.buf) held += nodeWeight(n);
    if (!this.afford((branches.length - 1) * held)) groups = this.cutAll(groups);
    for (const g of groups) this.gather(g);
    const merged = mergedGroups();
    for (const branch of branches) {
      for (const g of this.walk(seq(branch), groups.map(forkGroup))) mergeGroup(merged, g);
    }
    groups = merged.groups;
    for (const g of groups) dedupeTails(g);
    return groups;
  }

  walk(nodes, groups) {
    for (const node of nodes) {
      switch (node.tag) {
        case N_BRACE:
          groups = crosses(node) ? this.choice(node.branches, groups) : this.push(groups, node);
          break;
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

  text(g) {
    this.settle(g);
    if (g.doubleStar) {
      g.doubleStar = false;
      g.buf.push(STAR_NODE);
    }
  }

  settle(g) {
    if (!g.pendingGlobstar) return;
    this.gather(g);
    const t = g.tails[0];
    const lone = g.tails.length === 1 && t !== START && this.elems[t] === G0_ELEM;
    rebase(g, lone ? t : this.emit(G0_ELEM, g.tails, false), B_SEP);
  }

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

  afford(weight) {
    const units = weight * COPY_COST;
    if (units > this.room) return false;
    this.room -= units;
    return true;
  }

  closeAll(groups) {
    let joined = null;
    const starred = [null, null, null];
    for (const g of groups) {
      if (g.shadow && !g.doubleStar) {
        g.tails.length = 0;
        continue;
      }
      let into;
      if (g.doubleStar) {
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

  end(g) {
    if (g.shadow && !g.doubleStar) return;
    if (g.doubleStar) {
      const elem = g.state === B_START ? G0_ELEM : G1_ELEM;
      this.gather(g);
      this.link(this.emit(elem, g.tails, false), ACCEPT);
    } else if (g.state === B_GLUE && g.buf.length === 0) {
      for (const t of g.tails) this.link(t, ACCEPT);
    } else {
      this.settle(g);
      this.link(this.segment(g, false), ACCEPT);
    }
  }

  segment(g, cut) {
    this.gather(g);
    const elem = this.close(g.buf);
    const wild = elem.kind === EL_WILD;
    const glued = g.state === B_GLUE;
    const id = this.emit(elem, g.tails, glued);
    if (wild && (glued || cut)) this.pieces[id] = g.buf.slice();
    return id;
  }

  close(buf) {
    if (buf.some((n) => n.tag === N_LITERAL && hasEscapedSep(n.str))) {
      return makeElem(EL_WILD, null, makeWild(WK_AFFIX, { minLen: NEVER_LEN, variable: false }));
    }
    for (const n of buf) {
      if (n.tag !== N_LITERAL)
        return makeElem(EL_WILD, null, compileWild(buf, this.dot, this.ci, this));
    }
    return makeElem(EL_LIT, concatLits(buf, buf.length), null);
  }
}

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
