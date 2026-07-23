// Linear instruction tags and immutable zero-field op singletons.

export const OP_LIT = 0;
export const OP_ANYCHAR = 1;
export const OP_STAR = 2;
export const OP_CLASS = 3;
export const OP_SEP = 4;
export const OP_SEP_RUN = 5;
export const OP_GLOBSTAR = 6;
export const OP_OPT_SEGMENTS_SLASH = 7;
export const OP_SLASH_ANYTHING = 8;
export const OP_GLOBSTAR_ANY = 9;
export const OP_LEADING_SEPS = 10;
export const OP_ALTERNATION = 11;

export const ANYCHAR_OP = Object.freeze({ kind: OP_ANYCHAR });
export const STAR_OP = Object.freeze({ kind: OP_STAR });
export const SEP_OP = Object.freeze({ kind: OP_SEP });
export const SEP_RUN_OP = Object.freeze({ kind: OP_SEP_RUN });
export const GLOBSTAR_OP = Object.freeze({ kind: OP_GLOBSTAR });
export const OSS_OP = Object.freeze({ kind: OP_OPT_SEGMENTS_SLASH });
export const SLASH_ANY_OP = Object.freeze({ kind: OP_SLASH_ANYTHING });
export const GSTAR_ANY_OP = Object.freeze({ kind: OP_GLOBSTAR_ANY });
export const LEADING_SEPS_OP = Object.freeze({ kind: OP_LEADING_SEPS });

export function assertNormalizedProgram(ops) {
  let previousLit = false;
  let previousStar = false;
  for (const op of ops) {
    if (op.kind === OP_GLOBSTAR) throw new Error("raw globstar escaped lowering");
    if (op.kind === OP_LIT && previousLit) throw new Error("adjacent literal ops");
    if (op.kind === OP_STAR && previousStar) throw new Error("adjacent star ops");
    if (op.kind === OP_ALTERNATION) {
      for (const branch of op.branches) assertNormalizedProgram(branch);
    }
    previousLit = op.kind === OP_LIT;
    previousStar = op.kind === OP_STAR;
  }
}
