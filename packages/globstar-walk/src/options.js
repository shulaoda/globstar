// User-facing options for `glob` / `globSync`.

const DEFAULTS = {
  cwd: ".",
  dot: false,
  caseInsensitive: false,
  followSymlinks: true,
  ignore: [],
};

export function normalizeOptions(input) {
  // Only an absent or `undefined` value means "default" — `null` and
  // wrong types fall through to the walker's type checks.
  const o = input ?? {};
  const opt = (key) => (o[key] === undefined ? DEFAULTS[key] : o[key]);
  return {
    cwd: opt("cwd"),
    dot: opt("dot"),
    caseInsensitive: opt("caseInsensitive"),
    followSymlinks: opt("followSymlinks"),
    ignore: opt("ignore"),
  };
}

// Project user-facing options into the bag the matcher consumes.
export function toMatcherOptions(opts) {
  return {
    dot: opts.dot,
    caseInsensitive: opts.caseInsensitive,
  };
}
