export const N_CONCAT = 0;
export const N_LITERAL = 1;
export const N_SEPARATOR = 2;
export const N_ANYCHAR = 3;
export const N_STAR = 4;
export const N_GLOBSTAR = 5;
export const N_CLASS = 6;
export const N_BRACE = 7;

export const SEP_NODE = Object.freeze({ tag: N_SEPARATOR });
export const ANYCHAR_NODE = Object.freeze({ tag: N_ANYCHAR });
export const STAR_NODE = Object.freeze({ tag: N_STAR });
export const GLOBSTAR_NODE = Object.freeze({ tag: N_GLOBSTAR });

export function lit(str) {
  return { tag: N_LITERAL, str };
}
export function klass(neg, ranges) {
  return { tag: N_CLASS, neg, ranges };
}
export function brace(branches) {
  return { tag: N_BRACE, branches };
}
export function concat(children) {
  return { tag: N_CONCAT, children };
}
export function nodeToLiteral(n) {
  let out = "";
  for (const x of seq(n)) {
    if (x.tag === N_LITERAL) out += x.str;
    else if (x.tag === N_SEPARATOR) out += "/";
    else return null;
  }
  return out;
}
export function seq(n) {
  return n.tag === N_CONCAT ? n.children : [n];
}

function endsWithGlobstar(n) {
  if (n.tag === N_GLOBSTAR) return true;
  if (n.tag === N_CONCAT) return endsInGlobstar(n.children, n.children.length, false);
  return n.tag === N_BRACE && n.branches.some(endsWithGlobstar);
}
export function nullable(n) {
  if (n.tag === N_BRACE) return n.branches.some(nullable);
  return n.tag === N_CONCAT && n.children.every(nullable);
}
export function endsInGlobstar(nodes, end, outer) {
  for (let i = end - 1; i >= 0; i--) {
    if (endsWithGlobstar(nodes[i])) return true;
    if (!nullable(nodes[i])) return false;
  }
  return outer;
}
