// Type definitions for `@globstar/walk`.
//
// Filesystem walker built on the `@globstar/core` matcher: traverse
// a directory tree and return the file paths matching a glob set.

/**
 * Walker options. A key set to `undefined` keeps its default; an
 * unknown key, or a value of the wrong type, is a `TypeError`. Only own
 * keys are read.
 */
export interface GlobOptions {
  /**
   * Working directory the patterns and ignore globs resolve against,
   * resolved as `path.resolve` does (`""` is the current directory).
   * Default `"."`.
   */
  cwd?: string | undefined;
  /** Match dot-files (paths starting with `.`). Default `false`. */
  dot?: boolean | undefined;
  /** ASCII-case-insensitive byte comparison. Default `false`. */
  caseInsensitive?: boolean | undefined;
  /**
   * Follow symlinks when walking directories. Cycles are detected via
   * `fs.realpathSync.native` on the symlink target and skipped. When
   * `false`, symlinks are dropped entirely (neither emitted nor
   * descended); each entry is judged when its folder is read, so a folder
   * replaced by a symlink after that is read through the link. Default
   * `true` — matches tinyglobby's `followSymbolicLinks`.
   */
  followSymlinks?: boolean | undefined;
  /**
   * Extra patterns to exclude (in addition to any `!`-prefixed
   * entries). A bare string means a one-element list. An entry may not
   * start with `!` (it would ignore everything else): that is an
   * `InvalidPattern` error.
   */
  ignore?: string | readonly string[] | undefined;
}

/**
 * Concurrent async glob. Each discovered directory schedules its own
 * `fs.readdir` at once, so the reads overlap on libuv's threadpool and
 * the event loop is never blocked. It gains most when reads are slow (a
 * cold cache, macOS); with a warm cache on Linux {@link globSync} can be
 * faster. Measure where it matters.
 *
 * Multi-pattern combines via OR. `!`-prefixed patterns auto-split into
 * the ignore set by the parity of their leading `!`s (`!x` is ignored,
 * `!!x` is `x` again — globby reads `!!x` as an ignore too):
 *
 * ```ts
 * await glob("src/**", { cwd: projectRoot });
 * await glob(["src/**", "!src/generated/**"]);
 * await glob("src/**", { ignore: ["src/generated/**"] }); // same thing
 * ```
 *
 * Output is always **absolute file paths** rooted at the resolved
 * `cwd`. Directory matches aren't emitted, nor FIFOs, sockets or devices;
 * a pattern or `ignore` entry that ends in `/` matches folders only
 * (`ignore: ["node_modules/"]` prunes that folder). Callers wanting paths
 * relative to `cwd` should `path.relative(cwd, p)` at their boundary.
 * Errors reject the returned Promise with a {@link WalkError} (never
 * silent); wrongly-typed patterns or options reject it with a
 * `TypeError`. An option key explicitly set to `undefined` keeps its
 * default.
 */
export function glob(patterns: string | Iterable<string>, options?: GlobOptions): Promise<string[]>;

/**
 * Sync DFS glob: one directory at a time, no promise cost per
 * directory, and fewer listings held at once. Use it when blocking is
 * OK; with a warm cache it can beat {@link glob}.
 *
 * Same options, same `!`-split behavior, same file-only output.
 * Errors throw a {@link WalkError} synchronously; wrongly-typed
 * patterns or options throw a `TypeError`.
 *
 * ```ts
 * for (const p of globSync("src/**", { cwd: projectRoot })) {
 *   compile(p);
 * }
 * ```
 */
export function globSync(patterns: string | Iterable<string>, options?: GlobOptions): string[];

/**
 * Thrown by {@link glob} / {@link globSync}. Discriminate via
 * `.kind`:
 *
 * - `"InvalidPattern"` — a pattern failed to compile. `.pattern` is
 *   the offending pattern, `.reason` is the parser's message and
 *   `.cause` the `GlobError` itself. When the members of a list each
 *   compile alone but not together (they share one expansion budget),
 *   `.pattern` is the whole list, formatted like `["a", "b"]`.
 * - `"Io"` — a `readdir` / `stat` failed during traversal (missing
 *   `cwd`, EACCES, ENOENT on a vanished dir, …). `.path` is the
 *   directory we tried to read, `.cause` is the underlying Node error.
 *
 * The async path rejects the returned Promise; the sync path throws.
 *
 * ```ts
 * try {
 *   await glob("src/**", { cwd: "./does-not-exist" });
 * } catch (e) {
 *   if (e instanceof WalkError && e.kind === "Io") {
 *     console.error(`cannot read ${e.path}:`, e.cause?.message);
 *   } else {
 *     throw e;
 *   }
 * }
 * ```
 */
export class WalkError extends Error {
  readonly name: "WalkError";
  readonly kind: "InvalidPattern" | "Io";
  /** Path that triggered the error (set when `kind === "Io"`). */
  readonly path?: string;
  /** Pattern that failed to compile (set when `kind === "InvalidPattern"`). */
  readonly pattern?: string;
  /** Compile reason (set when `kind === "InvalidPattern"`). */
  readonly reason?: string;
  /** Underlying error: the Node error for `"Io"`, the `GlobError` for `"InvalidPattern"`. */
  readonly cause?: Error;
  constructor(kind: "InvalidPattern" | "Io", info?: Record<string, unknown>);
  /** The fields worth logging: `name`, `kind`, `message` and the set ones of `pattern`, `reason`, `path` (not `cause`). */
  toJSON(): {
    name: "WalkError";
    kind: "InvalidPattern" | "Io";
    message: string;
    pattern?: string;
    reason?: string;
    path?: string;
  };
}
