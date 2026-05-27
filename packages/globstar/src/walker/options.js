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

// Project user-facing options into the bag `globstar` consumes.
// Walker is the batch use case — one matcher fan-outs across hundreds-
// to-thousands of paths — so we opt into DFA (faster per-match, slower
// to compile, amortizes well). `globstar()` direct callers default to
// PikeVm via `compileMatcher` for single-shot speed. `opts.__engine`
// can still override (used by benches).
export function toMatcherOptions(opts) {
  return {
    dot: opts.dot,
    caseInsensitive: opts.caseInsensitive,
    __engine: opts.__engine ?? "dfa",
    // `__noAutoOr` is a bench-only escape hatch threaded through to
    // `compileMatcher` — used to measure the merged-engine path
    // independent of `Glob::union`'s NFA-size auto-routing.
    __noAutoOr: opts.__noAutoOr,
  };
}
