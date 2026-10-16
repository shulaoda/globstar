// User-facing options for `glob` / `globSync`.

const DEFAULTS = {
  cwd: ".",
  dot: false,
  caseInsensitive: false,
  followSymlinks: true,
  ignore: [],
};

// The options bag with every default filled in. Only own keys count
// (a key on `Object.prototype` changes nothing), a key set to
// `undefined` keeps its default, an unknown key or a non-boolean flag is
// a TypeError; `cwd` and `ignore` are checked where they are used.
export function normalizeOptions(input) {
  if (input === undefined || input === null) return { ...DEFAULTS };
  if (typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError(
      `options must be an object, got ${Array.isArray(input) ? "an array" : typeof input}`,
    );
  }
  for (const key of Object.keys(input)) {
    if (!Object.hasOwn(DEFAULTS, key)) throw new TypeError(`unknown option "${key}"`);
  }
  const opt = (key) =>
    Object.hasOwn(input, key) && input[key] !== undefined ? input[key] : DEFAULTS[key];
  const flag = (key) => {
    const v = opt(key);
    if (typeof v !== "boolean") {
      throw new TypeError(`${key} must be a boolean, got ${v === null ? "null" : typeof v}`);
    }
    return v;
  };
  return {
    cwd: opt("cwd"),
    dot: flag("dot"),
    caseInsensitive: flag("caseInsensitive"),
    followSymlinks: flag("followSymlinks"),
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
