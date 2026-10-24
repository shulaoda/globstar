const DEFAULTS = {
  cwd: ".",
  dot: false,
  caseInsensitive: false,
  followSymlinks: true,
  ignore: [],
};

export const typeName = (v) => (v === null ? "null" : typeof v);

// Only own keys count, `undefined` keeps the default, an unknown key or a
// value of the wrong type is a TypeError.
export function normalizeOptions(input) {
  input ??= {};
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
    if (typeof v !== "boolean") throw new TypeError(`${key} must be a boolean, got ${typeName(v)}`);
    return v;
  };
  const dot = flag("dot");
  const caseInsensitive = flag("caseInsensitive");
  const followSymlinks = flag("followSymlinks");
  const given = opt("ignore");
  const ignore = typeof given === "string" ? [given] : given;
  if (!Array.isArray(ignore)) {
    throw new TypeError(`ignore must be a string or an array of strings, got ${typeName(given)}`);
  }
  for (const p of ignore) {
    if (typeof p !== "string") {
      throw new TypeError(`ignore pattern must be a string, got ${typeName(p)}`);
    }
  }
  const cwd = opt("cwd");
  if (typeof cwd !== "string") throw new TypeError(`cwd must be a string, got ${typeName(cwd)}`);
  return { cwd, dot, caseInsensitive, followSymlinks, ignore };
}
