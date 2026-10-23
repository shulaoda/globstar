// Node tags. The parser never produces a concatenation of one node, nor one
// inside another (the empty one is an empty brace branch), and a brace
// always has two or more branches (§10.4). A globstar is a `**` with a
// separator, or the edge of the pattern, on both sides (§9.1), a star
// elsewhere: the parser emits a star for a `**` beside text, and a
// globstar for one beside a separator, a brace or an edge, which may be
// either in different brace expansions.
export const N_CONCAT = 0;
export const N_LITERAL = 1; // bytes without metacharacters, one char each; consecutive literals are merged
export const N_SEPARATOR = 2;
export const N_ANYCHAR = 3;
export const N_STAR = 4;
export const N_GLOBSTAR = 5;
export const N_CLASS = 6;
export const N_BRACE = 7;

// One frozen node each; union.js compares them by identity, cheaper than
// reading `tag` off nodes of several shapes.
export const SEP_NODE = Object.freeze({ tag: N_SEPARATOR });
export const ANYCHAR_NODE = Object.freeze({ tag: N_ANYCHAR });
export const STAR_NODE = Object.freeze({ tag: N_STAR });
export const GLOBSTAR_NODE = Object.freeze({ tag: N_GLOBSTAR });

// `str` holds the literal's bytes, one char per byte.
export function lit(str) {
  return { tag: N_LITERAL, str };
}
// `ranges`: inclusive `{ lo, hi }` byte ranges; a lone byte `b` is `b..b`.
export function klass(neg, ranges) {
  return { tag: N_CLASS, neg, ranges };
}
export function brace(branches) {
  return { tag: N_BRACE, branches };
}
export function concat(children) {
  return { tag: N_CONCAT, children };
}

// The bytes of a pure-literal body, one char each, or `null`. A sequence
// holds no sequence.
export function nodeToLiteral(n) {
  if (n.tag === N_LITERAL) return n.str;
  let out = "";
  for (const x of seq(n)) {
    if (x.tag === N_LITERAL) out += x.str;
    else if (x.tag === N_SEPARATOR) out += "/";
    else return null;
  }
  return out;
}

// The node as a sequence: a concat's children, or the node alone.
export function seq(n) {
  return n.tag === N_CONCAT ? n.children : [n];
}

// Does the node end in a `**` in some expansion, with only what may read
// as nothing after it? Such a `**` reads the `/` after the node as its
// `**/`.
function endsWithGlobstar(n) {
  if (n.tag === N_GLOBSTAR) return true;
  if (n.tag === N_CONCAT) return endsInGlobstar(n.children, n.children.length, false);
  return n.tag === N_BRACE && n.branches.some(endsWithGlobstar);
}

// May the node read as nothing (a brace with an empty expansion)?
export function nullable(n) {
  if (n.tag === N_BRACE) return n.branches.some(nullable);
  return n.tag === N_CONCAT && n.children.every(nullable);
}

// `endsWithGlobstar` for the sequence `nodes[..end]`: `outer` when every
// node in it may read as nothing, so that what comes before it decides.
export function endsInGlobstar(nodes, end, outer) {
  for (let i = end - 1; i >= 0; i--) {
    if (endsWithGlobstar(nodes[i])) return true;
    if (!nullable(nodes[i])) return false;
  }
  return outer;
}
