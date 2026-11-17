import { hasEscapedSep } from "../bytes.js";
import { room } from "./graph.js";
import { N_BRACE, N_LITERAL, N_SEPARATOR, endsInGlobstar, seq } from "../../parse/ast.js";

const MAX_SUFFIX_SET = 16;

const MAX_HEAD_DEPTH = 64;

export function suffixes(body) {
  const nodes = seq(body);
  const suffix = trailing(nodes, nodes.length, false);
  if (suffix.length > 0) return [suffix];
  return suffixSet(nodes);
}

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

function suffixSet(nodes) {
  if (nodes.length === 0) return [];
  const last = nodes[nodes.length - 1];
  if (last.tag !== N_BRACE) return [];
  const common = trailing(nodes, nodes.length - 1, false);
  const afterGlobstar = endsInGlobstar(nodes, nodes.length - 1, false);
  const many = last.branches.length > MAX_SUFFIX_SET;
  const set = [];
  let shared = null;
  for (const branch of last.branches) {
    const b = seq(branch);
    let own = trailing(b, b.length, afterGlobstar);
    if (!many && b.every((n) => n.tag === N_LITERAL || n.tag === N_SEPARATOR)) own = common + own;
    if (own.length === 0) return [];
    if (!many) {
      set.push(own);
      continue;
    }
    if (shared === null) shared = own;
    else {
      let k = 0;
      while (
        k < shared.length &&
        k < own.length &&
        shared[shared.length - 1 - k] === own[own.length - 1 - k]
      )
        k++;
      shared = shared.slice(shared.length - k);
    }
  }
  if (shared === null) return set;
  return shared.length === 0 ? [] : [shared];
}

export function staticPrefixes(body, weight) {
  const nodes = seq(body);
  let heads = readAt(nodes, 0, weight);
  if (heads.copied > heads.limit) heads = readAt(nodes, MAX_HEAD_DEPTH, weight);
  return dedupe(heads.out);
}

function readAt(nodes, depth, weight) {
  const heads = { spent: 0, limit: room(weight), copied: 0, out: [] };
  readHeads(heads, nodes, 0, null, "", 0, depth);
  return heads;
}

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
      end =
        node.tag === N_BRACE && afford(heads, node.branches.length) && opensSegment(node)
          ? acc.length
          : boundary;
      break;
    }
  }
  while (end > 0 && acc.charCodeAt(end - 1) === 0x2f) end--;
  heads.copied += acc.length;
  if (heads.copied <= heads.limit) heads.out.push(acc.slice(0, end));
  else heads.out = [""];
}

const covered = (heads) => heads.out.length > 0 && heads.out[heads.out.length - 1].length === 0;

function afford(heads, n) {
  if (heads.spent + n > heads.limit) return false;
  heads.spent += n;
  return true;
}

function opensSegment(node) {
  return node.branches.every((b) => seq(b)[0]?.tag === N_SEPARATOR);
}

function dedupe(prefixes) {
  if (prefixes.length <= 1) return prefixes;
  prefixes.sort();
  if (prefixes[0].length === 0) return [""];
  const kept = [];
  const up = [];
  let top = -1;
  for (let i = 0; i < prefixes.length; i++) {
    const p = prefixes[i];
    while (top !== -1 && !p.startsWith(kept[top])) top = up[top];
    if (top !== -1 && (p.length === kept[top].length || p.charCodeAt(kept[top].length) === 0x2f)) {
      continue;
    }
    up.push(top);
    top = kept.length;
    kept.push(p);
  }
  return kept.sort((a, b) => a.length - b.length);
}
