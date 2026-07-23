// Type definitions for `@globstar/walk`.
//
// Filesystem walker built on the `@globstar/core` matcher: traverse
// a directory tree and return the file paths matching a glob set.

export interface GlobOptions {
  /** Working directory the patterns and ignore globs resolve against. Default `"."`. */
  cwd?: string;
  /** Match dot-files (paths starting with `.`). Default `false`. */
  dot?: boolean;
  /** ASCII-case-insensitive byte comparison. Default `false`. */
  caseInsensitive?: boolean;
  /**
   * Follow symlinks when walking directories. Cycles are detected via
   * `fs.realpathSync` on the symlink target and skipped. When `false`,
   * symlinks are dropped entirely (neither emitted nor descended).
   * Default `true` — matches tinyglobby's `followSymbolicLinks`.
   */
  followSymlinks?: boolean;
  /** Extra patterns to exclude (in addition to any `!`-prefixed entries). */
  ignore?: readonly string[];
}

/**
 * Concurrent async glob. Each discovered directory schedules its own
 * `fs.readdir` callback immediately, so syscalls overlap on libuv's
 * threadpool — the right choice for any non-trivial corpus.
 *
 * Multi-pattern combines via OR. `!`-prefixed patterns auto-split into
 * the ignore set (globby-style):
 *
 * ```ts
 * await glob("**​/*.ts", { cwd: "./src" });
 * await glob(["**​/*.ts", "!**​/*.test.ts"]);
 * await glob("**​/*.ts", { ignore: ["**​/*.test.ts"] });
 * ```
 *
 * Output is always **absolute file paths** rooted at the resolved
 * `cwd`. Directory matches aren't emitted. Callers wanting paths
 * relative to `cwd` should `path.relative(cwd, p)` at their boundary.
 * Errors throw a {@link WalkError} (the returned Promise rejects;
 * never silent).
 */
export function glob(
  patterns: string | readonly string[],
  options?: GlobOptions,
): Promise<string[]>;

/**
 * Sync DFS glob. Use when blocking traversal is OK and the corpus is
 * small enough that I/O concurrency wouldn't pay for itself (a few
 * hundred files or fewer). For everything else prefer {@link glob}.
 *
 * Same options, same `!`-split behavior, same file-only output.
 * Errors throw a {@link WalkError} synchronously.
 *
 * ```ts
 * for (const p of globSync("**​/*.ts", { cwd: "./src" })) {
 *   compile(p);
 * }
 * ```
 */
export function globSync(patterns: string | readonly string[], options?: GlobOptions): string[];

/**
 * Thrown by {@link glob} / {@link globSync}. Discriminate via
 * `.kind`:
 *
 * - `"InvalidPattern"` — a pattern failed to compile. `.pattern` is
 *   the offending pattern (or comma-joined list), `.reason` is the
 *   parser's message.
 * - `"Io"` — a `readdir` / `stat` failed during traversal (missing
 *   `cwd`, EACCES, ENOENT on a vanished dir, …). `.path` is the
 *   directory we tried to read, `.cause` is the underlying Node error.
 *
 * The async path rejects the returned Promise; the sync path throws.
 *
 * ```ts
 * try {
 *   await glob("**​/*.ts", { cwd: "./does-not-exist" });
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
  /** Underlying Node error (set when `kind === "Io"`). */
  readonly cause?: Error;
  constructor(kind: "InvalidPattern" | "Io", info?: Record<string, unknown>);
}
