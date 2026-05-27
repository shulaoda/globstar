// Errors produced by `glob` / `globSync`. `InvalidPattern` covers
// compile-time pattern failures; `Io` covers readdir/stat failures
// during traversal (sync throws, async rejects).

export class WalkError extends Error {
  constructor(kind, info) {
    super(formatMessage(kind, info));
    this.name = "WalkError";
    this.kind = kind;
    if (info) Object.assign(this, info);
  }
}

function formatMessage(kind, info) {
  switch (kind) {
    case "InvalidPattern":
      return `invalid pattern '${info.pattern}': ${info.reason}`;
    case "Io":
      return `walker error at ${info.path}: ${info.cause?.message ?? "io error"}`;
    default:
      return `unknown walker error: ${kind}`;
  }
}
