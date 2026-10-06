// Which `**` are globstars (GLOB_SPEC §7.0, §8.1).
//
// A pattern means the union of its brace expansions, and in each one a `**`
// is a globstar only with a separator, or the edge of the pattern, on both
// sides; otherwise it is a star. The parser decides a `**` beside plain
// tokens as it reads, and leaves one beside a brace, or at the edge of a
// branch, to this pass, which runs only then. Such a `**` meets a different
// token in every branch, so the braces are first taken apart by
// `distribute`; `mayTouch` tells when that can be the case. Then `decide`
// turns every `**` into a globstar or a star by the tokens beside it.

import { N_SEPARATOR, N_BRACE, N_CONCAT, N_GLOBSTAR, star } from "./ast.js";
import { distribute } from "./distribute.js";

// `budget.left` is what distribution may still copy (§7.7).
export function resolveGlobstars(body, budget) {
  if (mayTouch(body)) body = distribute(body, budget);
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
