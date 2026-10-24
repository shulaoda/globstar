// `InvalidPattern` at compile time; `Io` during the walk (sync throws,
// async rejects).
export class WalkError extends Error {
  constructor(kind, info) {
    super(formatMessage(kind, info ?? {}));
    this.name = "WalkError";
    this.kind = kind;
    if (info) Object.assign(this, info);
  }

  // `message` kept, the raw `cause` dropped.
  toJSON() {
    const out = { name: this.name, kind: this.kind, message: this.message };
    if (this.pattern !== undefined) out.pattern = this.pattern;
    if (this.reason !== undefined) out.reason = this.reason;
    if (this.path !== undefined) out.path = this.path;
    return out;
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
