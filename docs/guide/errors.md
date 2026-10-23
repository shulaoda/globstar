# Compile-time errors and limits

This page lists every way a pattern can fail to compile, the four size limits, and how to catch the error in JS and in Rust. Errors only happen when you compile. Once a pattern compiles, matching a path never fails with a glob error.

## When errors happen

Patterns are checked when you compile them: `compileMatcher()` and `globstar()` in JS, `Glob::new`, `Glob::new_with`, `Glob::union` and `Glob::union_with` in Rust. If you pass a list, the first bad pattern stops the whole compile and nothing is built.

globstar fails loudly on real mistakes, such as an unclosed `[` or `{`. It does not fail on things that have a sensible plain meaning. These all compile:

| Pattern       | What it means                               | Example path that matches |
| ------------- | ------------------------------------------- | ------------------------- |
| `a]b`         | a `]` with no open `[` is a literal byte    | `a]b`                     |
| `{a,}}`       | a `}` with no open `{` is a literal byte    | `}`                       |
| `file(1).txt` | `(` and `)` are always literal              | `file(1).txt`             |
| `{a}`         | a brace with one branch is literal text     | `{a}`                     |
| `src/a**b.ts` | a `**` that is not a whole name acts as `*` | `src/axyb.ts`             |
| `\a`          | escaping an ordinary byte gives that byte   | `a`                       |
| `[a-]`        | a `-` at the end of a class is a member     | `-`                       |

See [Pattern syntax](./syntax.md) for the full rules.

Spec: [GLOB_SPEC §6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Error kinds

The JS `GlobError` has a `.kind` string. The Rust `GlobError` is an enum with a variant of the same name. Some kinds carry extra fields: in JS they are properties on the error object, in Rust they are fields of the variant.

| Kind                  | When it happens                                                                              | Example pattern                     | Extra fields        |
| --------------------- | -------------------------------------------------------------------------------------------- | ----------------------------------- | ------------------- |
| `Empty`               | The pattern is empty, or has nothing after its leading `!`s.                                 | `""`, `!`, `!!`                     | –                   |
| `TooLong`             | The pattern is longer than 65,536 bytes.                                                     | `"a".repeat(65537)`                 | `len`, `max`        |
| `UnterminatedClass`   | A `[` has no `]` before the next `/` or the end. `[]`, `[!]` and `[/]` are cases of this.    | `src/[a-z.ts`, `[]`, `[/]`          | `at`                |
| `UnterminatedBrace`   | A `{` has no matching `}`.                                                                   | `src/{a,b.ts`                       | `at`                |
| `TrailingBackslash`   | The pattern ends with a single `\`.                                                          | `foo\`                              | –                   |
| `EscapedSeparator`    | The pattern contains `\/`. No file name can hold a `/`, so it could never match.             | `a\/b`                              | `at`                |
| `BraceNestingTooDeep` | Braces are nested more than 32 levels deep. Every `{` counts, even one that ends up literal. | `{a,` × 33, then `b`, then `}` × 33 | `max`               |
| `InvalidRange`        | A class range ends below where it starts.                                                    | `[z-a]`, `[a-A]`                    | `at`, `low`, `high` |
| `EmptyPatternSet`     | A pattern list is empty.                                                                     | `compileMatcher([])`                | –                   |
| `NegatedInUnion`      | Rust only: a negated pattern (an odd number of leading `!`) was given to `Glob::union`.      | `Glob::union(["src/**", "!*.log"])` | `index`, `pattern`  |

What the fields hold:

- `at` is a **byte** offset into the UTF-8 pattern, not a JS string index. For `src/[a-z.ts` it is `4`; for `é/[a` it is `3`, because `é` is two bytes. For `InvalidRange` it is where the range starts: `8` in `src/[abcz-a].ts`.
- `low` and `high` are the two range ends as byte values. `[a-A]` gives `low: 97`, `high: 65`.
- `len` is the pattern length in bytes, and `max` is the limit that was passed.
- `index` and `pattern` (Rust only) tell you which list member was negated.

When a pattern **list** fails because one member does not compile, the error says which member, counting every member given: in JS the error keeps its `kind` and gains `index`, and its message starts with `pattern 2: `; in Rust it is `GlobError::InMember { index, error }`, and `err.root()` is the error inside. Offsets count in that member.

`[a-A]` fails even with `caseInsensitive: true`. Case folding only changes how bytes compare when matching; it never changes what counts as a valid pattern. See [How a path is matched](./matching.md#caseinsensitive).

Spec: [GLOB_SPEC §6, §6.1](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## The two limits

The limits stop a hostile or broken pattern from using huge amounts of memory or time. Normal patterns never get close.

| Limit          | Value        | Rust constant       | Error                 |
| -------------- | ------------ | ------------------- | --------------------- |
| Pattern length | 65,536 bytes | `MAX_PATTERN_LEN`   | `TooLong`             |
| Brace nesting  | 32 levels    | `MAX_BRACE_NESTING` | `BraceNestingTooDeep` |

- **Pattern length** counts UTF-8 bytes, not characters. A pattern of 32,768 `é` (65,536 bytes) compiles; one more `é` is too long. The walkers count a pattern or `ignore` entry as it is written, before they take off a leading `/` or `!`, so `/` repeated 100,000 times and then `x` is too long there too; the error shows the first 64 characters of such a pattern.
- **Brace nesting** counts how many braces are open inside each other, including a `{` with no comma that ends up literal. Braces side by side do not nest: `**` followed by `{,/a}` 33 times compiles.

Nothing else is bounded. Within these two limits every pattern compiles, and its compile time and memory grow with its length, whatever its braces: braces next to a `**` are not expanded, and braces whose branches cross a `/`, like `{a,b,/}` repeated 100 times, split segments that globstar matches piece by piece instead of copying. A list compiles as one pattern, so a list of members that each compile compiles.

The limits are exported: in Rust as `globstar::error::{MAX_PATTERN_LEN, MAX_BRACE_NESTING}`, in JS as `MAX_PATTERN_LEN` and `MAX_BRACE_NESTING` from `@globstar/core`.

Spec: [GLOB_SPEC §6.2, §6](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)

## Catching errors in JS

`compileMatcher()` and `globstar()` throw a `GlobError`. Check it with `instanceof` and branch on `.kind`:

```js
import { compileMatcher, GlobError } from "@globstar/core";

try {
  compileMatcher("src/[a-z.ts");
} catch (err) {
  if (!(err instanceof GlobError)) throw err;
  console.log(err.kind); // 'UnterminatedClass'
  console.log(err.at); // 4
  console.log(err.message);
  // 'unterminated character class at byte 4: no `]` before a `/` or the end of the pattern'
}
```

A pattern or path that is not a string throws a plain `TypeError`, not a `GlobError`. This is the only thing `match()` can throw, short of a non-ASCII path too long for the runtime to hold as bytes (hundreds of megabytes), which fails with the runtime's own error.

## Catching errors in Rust

Compiling returns `Result<Glob, GlobError>`. `GlobError` implements `Display` and `std::error::Error`, so `?` works. Match on the variants when you want to react to one kind:

```rust
use globstar::{Glob, GlobError};

match Glob::new("src/[a-z.ts") {
    Ok(glob) => println!("{}", glob.is_match(b"src/a.ts")),
    Err(GlobError::UnterminatedClass { at }) => eprintln!("unclosed `[` at byte {at}"),
    Err(err) => eprintln!("bad pattern: {err}"),
}
```

## Lists of patterns: JS and Rust differ

Both languages reject an empty list with `EmptyPatternSet`. A `!` member is accepted in JS, where it adds its complement to the OR, and rejected in Rust: `Glob::union(["src/**", "!**/*.test.ts"])` returns `GlobError::NegatedInUnion { index: 1, pattern: "!**/*.test.ts" }`. Either way a `!` in a list never excludes anything; see [Negated members](./multi-pattern.md#negated-members).

Spec: [GLOB_SPEC §6, §16](https://github.com/shulaoda/globstar/blob/main/references/spec/GLOB_SPEC.md)
