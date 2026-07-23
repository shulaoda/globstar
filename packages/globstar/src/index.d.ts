// Type definitions for `@globstar/core`.
//
// Pure glob matcher — compile patterns into path predicates. No
// filesystem access, no `node:` imports; runs in any JS runtime.
// The filesystem walker lives in `@globstar/walk`.

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
 * ignores is the walker layer's job, not the matcher's.
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
 * Result of {@link Matcher.matchDir} — what a directory path means
 * for the pattern set. Walkers consult this per-directory to decide
 * whether to yield the dir, descend into it, or prune the subtree.
 * Mirrors the Rust crate's `DirMatch`.
 */
export type DirMatchValue = 0 | 1 | 2 | 3;

export declare const DirMatch: {
  readonly Pruned: 0;
  readonly Descend: 1;
  readonly Match: 2;
  readonly DescendAndMatch: 3;
  /** `Match` or `DescendAndMatch`. */
  isMatch(d: DirMatchValue): boolean;
  /** `Descend` or `DescendAndMatch`. */
  shouldDescend(d: DirMatchValue): boolean;
  isPruned(d: DirMatchValue): boolean;
  /** Combine the exact-match and prefix-match axes into one value. */
  fromExactPrefix(exact: boolean, prefix: boolean): DirMatchValue;
};

/** Compiled pattern set returned by {@link compileMatcher}. */
export interface Matcher {
  /** Full-path match — same predicate {@link globstar} returns. */
  match(input: string | Uint8Array): boolean;
  /** Directory-level verdict for walker pruning (see {@link DirMatch}). */
  matchDir(input: string | Uint8Array): DirMatchValue;
  /** Literal path prefixes a walker can seed traversal from. */
  staticPrefixes(): Uint8Array[];
}

/**
 * Compile one or more glob patterns into a {@link Matcher} — the
 * walker-facing surface with directory pruning (`matchDir`) and
 * traversal seeding (`staticPrefixes`) alongside the plain `match`
 * predicate. `@globstar/walk` is built on this.
 *
 * Throws a {@link GlobError} if any pattern fails to compile.
 */
export function compileMatcher(
  patterns: string | readonly string[],
  options?: GlobstarOptions,
): Matcher;

/**
 * Thrown by {@link globstar} / {@link compileMatcher} when a pattern
 * fails to compile. `.kind` carries the specific failure mode.
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
