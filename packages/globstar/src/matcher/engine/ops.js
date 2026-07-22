// Stable facade for the shared compiler IR. Implementation details live in
// `ops/` so IR shape, normalization, lowering, and prefix analysis can evolve
// independently without churning matcher imports.

export {
  OP_LIT,
  OP_ANYCHAR,
  OP_STAR,
  OP_CLASS,
  OP_SEP,
  OP_SEP_RUN,
  OP_GLOBSTAR,
  OP_OPT_SEGMENTS_SLASH,
  OP_SLASH_ANYTHING,
  OP_GLOBSTAR_ANY,
  OP_LEADING_SEPS,
  OP_ALTERNATION,
} from "./ops/ir.js";
export { lower } from "./ops/lower.js";
export { computeStaticPrefixes, dedupePrefixes } from "./ops/prefixes.js";
