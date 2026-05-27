// Type definitions for `@globstar/globstar`.
//
// Two layers:
//   - Walker  (`glob` / `globSync`): traverse the filesystem and
//                                     return matching file paths.
//   - Matcher (`globstar`):           compile patterns into a
//                                     `(input) => boolean` predicate.

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

export interface GlobstarOptions {
  /** Match dot-files. Default `true` at the matcher layer. */
  dot?: boolean;
  /** ASCII-case-insensitive byte comparison. Default `false`. */
  caseInsensitive?: boolean;
}

/**
 * Compile one or more glob patterns into a path predicate (no
 * filesystem access — pure string matching).
 *
 * Multi-pattern combines via OR; each pattern's own `!`-prefix
 * negation applies independently. Auto-splitting `!`-patterns into
 * ignores is the {@link glob} layer's job, not the matcher's.
 *
 * ```ts
 * const m = globstar("**​/*.ts");
 * m("foo.ts");                         // true
 * m("foo.md");                         // false
 *
 * // Plug into Array.filter directly — same shape as picomatch:
 * paths.filter(globstar(["**​/*.ts", "!**​/*.test.ts"]));
 *
 * // Single negated pattern flips the predicate:
 * const notTest = globstar("!**​/*.test.ts");
 * notTest("a.ts");          // true
 * notTest("a.test.ts");     // false
 * ```
 *
 * Throws a {@link GlobError} if any pattern fails to compile.
 */
export function globstar(
  patterns: string | readonly string[],
  options?: GlobstarOptions,
): (input: string | Uint8Array) => boolean;

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

/**
 * Thrown by {@link globstar} when a pattern fails to compile.
 * `.kind` carries the specific failure mode.
 *
 * ```ts
 * try {
 *   globstar("[unclosed");
 * } catch (e) {
 *   if (e instanceof GlobError) console.error(e.kind, e.message);
 * }
 * ```
 */
export class GlobError extends Error {
  readonly name: "GlobError";
  readonly kind:
    | "Empty"
    | "TooLong"
    | "UnterminatedClass"
    | "UnterminatedBrace"
    | "TrailingBackslash"
    | "BraceNestingTooDeep"
    | "InvalidRange"
    | "EmptyPatternSet";
  constructor(kind: GlobError["kind"], info?: Record<string, unknown>);
}
