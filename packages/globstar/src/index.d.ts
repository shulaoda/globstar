// Type definitions for `@globstar/core`.
//
// Pure glob matcher — compile patterns into path predicates. No
// filesystem access, no `node:` imports; runs in any JS runtime.
// The filesystem walker lives in `@globstar/walk`.

/**
 * Matcher options. A key set to `undefined` keeps its default; an
 * unknown key, or a flag that is not a boolean, throws a `TypeError`.
 * Only own keys are read.
 */
export interface GlobstarOptions {
  /** Match dot-files. Default `true` at the matcher layer. */
  dot?: boolean | undefined;
  /** ASCII-case-insensitive byte comparison. Default `false`. */
  caseInsensitive?: boolean | undefined;
}

/**
 * Compile one or more glob patterns into a path predicate (no
 * filesystem access — pure string matching).
 *
 * Multi-pattern combines via OR (a pattern given twice counts once);
 * each pattern's own `!`-prefix
 * negation applies independently — a negated member contributes its
 * COMPLEMENT to the union, it does not subtract from the other
 * members. `["src/**", "!*.test.ts"]` therefore matches (almost)
 * everything. Include/exclude filtering is the walker layer's job
 * (`@globstar/walk` auto-splits `!`-patterns into its ignore set).
 *
 * ```ts
 * const m = globstar("src/**");
 * m("src/foo.ts");                     // true
 * m("lib/foo.ts");                     // false
 *
 * // Plug into Array.filter directly:
 * paths.filter(globstar(["src/**", "lib/**"]));
 *
 * // Single negated pattern flips the predicate:
 * const notTest = globstar("!*.test.ts");
 * notTest("a.ts");          // true
 * notTest("a.test.ts");     // false
 * ```
 *
 * Throws a {@link GlobError} if any pattern fails to compile (a list
 * member's error carries its `index`), and a `TypeError` if a pattern
 * is not a string or holds a lone surrogate, or the options are not a
 * plain object of known boolean flags.
 */
export function globstar(
  patterns: string | Iterable<string>,
  options?: GlobstarOptions,
): (input: string) => boolean;

/**
 * Result of {@link Matcher.matchDir} — what a directory path means
 * for the pattern set. The names read from the pattern's side; a walker
 * reads the three facts (`matches`, `mayMatchBelow`, `matchesAllBelow`)
 * according to the role the pattern plays. Mirrors the Rust crate's
 * `DirMatch`.
 */
export type DirMatchValue = 0 | 1 | 2 | 3 | 4 | 5;

/** The directory matches; nothing below it can (a selecting walker yields it and stops). */
export declare const MATCH: 0;
/** Neither the directory nor anything below it matches (a selecting walker prunes it; an excluding one stops testing here). */
export declare const PRUNED: 1;
/** Directory itself doesn't match, but some descendant might; descend. */
export declare const DESCEND: 2;
/** Directory matches AND some descendant might too; yield and descend. */
export declare const DESCEND_AND_MATCH: 3;
/** Directory itself doesn't match, but EVERY path below it does. */
export declare const DESCEND_ALL: 4;
/** Directory matches AND every path below it does. */
export declare const DESCEND_ALL_AND_MATCH: 5;

export declare const DirMatch: {
  readonly Match: 0;
  readonly Pruned: 1;
  readonly Descend: 2;
  readonly DescendAndMatch: 3;
  readonly DescendAll: 4;
  readonly DescendAllAndMatch: 5;
  /** The verdict with these facts; `allBelow` implies `mayBelow`. */
  from(matches: boolean, mayBelow: boolean, allBelow: boolean): DirMatchValue;
  /** Does the directory's own path match? */
  matches(d: DirMatchValue): boolean;
  /**
   * May some path below the directory match? `false` is a guarantee
   * that none does: a walker selecting files can skip the directory,
   * and one excluding files needs no more tests below it.
   */
  mayMatchBelow(d: DirMatchValue): boolean;
  /**
   * `DescendAll` or `DescendAllAndMatch` — every path below the
   * directory matches: `match(dir === "" ? s : dir + "/" + s)` holds for
   * every `s` of one or more non-empty segments. `true` is a guarantee; `false` means
   * "not all, or not provable". Always `false` under `dot: false`, where
   * no wildcard matches the dot-led names below.
   *
   * A walker excluding files may skip the directory whole only when
   * this holds.
   */
  matchesAllBelow(d: DirMatchValue): boolean;
};

/** Compiled pattern set returned by {@link compileMatcher}. */
export interface Matcher {
  /**
   * Full-path match — same predicate {@link globstar} returns. The path
   * is matched as given: with `dot: true` the names `.` and `..` are
   * plain bytes, so `src/**` matches `src/../secret.txt`; normalize
   * paths from outside first. Throws a `TypeError` on non-string input.
   */
  match(input: string): boolean;
  /**
   * Directory-level verdict for walker pruning (see {@link DirMatch}).
   * Ask about a directory without a trailing separator (`src`, not
   * `src/`; `""` for the root). A negated member matches every directory
   * outside its sublanguage and always lets the walk go on, so with one
   * the verdict is never `PRUNED` or `MATCH`; the positive members still
   * give the all-below bit. Throws a `TypeError` on non-string input.
   */
  matchDir(input: string): DirMatchValue;
  /**
   * Literal path prefixes a walker can seed traversal from: for each
   * expansion of the pattern's braces, its literal head up to its last
   * whole segment, with heads a shorter one covers removed
   * (`{src,lib}/index.ts` gives `src/index.ts` and `lib/index.ts`). The
   * heads are UTF-8 bytes spelled with `/` and in the pattern's case:
   * every match starts with one of them, except that with
   * `caseInsensitive` a match may differ in case, and on Windows in its
   * separators. An empty prefix means the root. Past a size bound, a
   * head is a shorter prefix that still holds (GLOB_SPEC §17).
   */
  staticPrefixes(): Uint8Array[];
}

/**
 * Compile one or more glob patterns into a {@link Matcher} — the
 * walker-facing surface with directory pruning (`matchDir`) and
 * traversal seeding (`staticPrefixes`) alongside the plain `match`
 * predicate. `@globstar/walk` is built on this.
 *
 * Throws a {@link GlobError} if any pattern fails to compile (a list
 * member's error carries its `index`), and a `TypeError` if a pattern
 * is not a string or holds a lone surrogate, or the options are not a
 * plain object of known boolean flags.
 */
export function compileMatcher(
  patterns: string | Iterable<string>,
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
    | "EscapedSeparator"
    | "BraceNestingTooDeep"
    | "InvalidRange"
    | "EmptyPatternSet";
  /** Byte offset of the offending construct (kind-dependent; for "InvalidRange" the range's start). */
  readonly at?: number;
  /** Pattern length for "TooLong". */
  readonly len?: number;
  /** The limit that was passed, for "TooLong" and "BraceNestingTooDeep". */
  readonly max?: number;
  /** Range bounds for "InvalidRange". */
  readonly low?: number;
  readonly high?: number;
  /**
   * For an error from one member of a pattern list: the member's position,
   * counting every member given. Offsets count in that member.
   */
  readonly index?: number;
  constructor(kind: GlobError["kind"], info?: Record<string, unknown>);
  /** The kind, the message and the set fields, for logging. */
  toJSON(): {
    name: "GlobError";
    kind: GlobError["kind"];
    message: string;
    at?: number;
    len?: number;
    max?: number;
    low?: number;
    high?: number;
    index?: number;
  };
}

/** The longest pattern accepted, in bytes (`"TooLong"`). */
export declare const MAX_PATTERN_LEN: 65536;
/** How deep braces may nest (`"BraceNestingTooDeep"`). */
export declare const MAX_BRACE_NESTING: 32;
