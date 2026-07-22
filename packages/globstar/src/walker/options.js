// User-facing options for `glob` / `globSync`.

const DEFAULTS = {
  cwd: ".",
  dot: false,
  caseInsensitive: false,
  followSymlinks: true,
  ignore: [],
};

export function normalizeOptions(input) {
  return input == null ? { ...DEFAULTS } : { ...DEFAULTS, ...input };
}

// Project user-facing options into the bag the matcher consumes.
export function toMatcherOptions(opts) {
  return {
    dot: opts.dot,
    caseInsensitive: opts.caseInsensitive,
  };
}
