// Node tags. The parser never produces a concatenation of one node, nor one
// inside another (the empty one is an empty brace branch); it emits a
// globstar only where the `**` owns a whole segment in every brace expansion
// (§9.1), and a brace always has two or more branches (§10.4).
export const N_CONCAT = 0;
export const N_LITERAL = 1; // bytes without metacharacters, one char each; consecutive literals are merged
export const N_SEPARATOR = 2;
export const N_ANYCHAR = 3;
export const N_STAR = 4;
export const N_GLOBSTAR = 5;
export const N_CLASS = 6;
export const N_BRACE = 7;

const SEP_NODE = Object.freeze({ tag: N_SEPARATOR });
const ANYCHAR_NODE = Object.freeze({ tag: N_ANYCHAR });
const STAR_NODE = Object.freeze({ tag: N_STAR });
const GLOBSTAR_NODE = Object.freeze({ tag: N_GLOBSTAR });

// `str` holds the literal's bytes, one char per byte.
export function lit(str) {
  return { tag: N_LITERAL, str };
}
export function sep() {
  return SEP_NODE;
}
export function anyChar() {
  return ANYCHAR_NODE;
}
export function star() {
  return STAR_NODE;
}
export function globstar() {
  return GLOBSTAR_NODE;
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

// Does the node end in a `**` (in every expansion of a brace, in some)?
// Such a `**` reads the `/` after the node as its `**/`.
export function endsWithGlobstar(n) {
  if (n.tag === N_GLOBSTAR) return true;
  if (n.tag === N_CONCAT) return n.children.length > 0 && endsWithGlobstar(n.children.at(-1));
  return n.tag === N_BRACE && n.branches.some(endsWithGlobstar);
}
