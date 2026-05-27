// Defense-in-depth limits for adversarial input. Both align with the
// Rust crate's GlobError limits (`spec/GLOB_SPEC.md` §10).
export const MAX_PATTERN_LEN = 64 * 1024;
export const MAX_BRACE_NESTING = 32;

export class GlobError extends Error {
  constructor(kind, info) {
    super(formatMessage(kind, info));
    this.name = "GlobError";
    this.kind = kind;
    if (info) Object.assign(this, info);
  }
}

function formatMessage(kind, info) {
  switch (kind) {
    case "Empty":
      return "empty pattern";
    case "TooLong":
      return `pattern too long: ${info.len} > ${info.max}`;
    case "UnterminatedClass":
      return `unterminated character class at byte ${info.at}`;
    case "UnterminatedBrace":
      return `unterminated brace expansion at byte ${info.at}`;
    case "TrailingBackslash":
      return "pattern ends with lone backslash";
    case "BraceNestingTooDeep":
      return `brace nesting exceeds limit ${info.max}`;
    case "InvalidRange":
      return `invalid character class range ${info.low}..${info.high} at byte ${info.at}`;
    case "EmptyPatternSet":
      return "globstar requires at least one pattern";
    default:
      return `unknown glob error: ${kind}`;
  }
}
