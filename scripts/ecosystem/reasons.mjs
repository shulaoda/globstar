// Why a row's answer differs from the library whose suite it came from:
// a reason id, a title and a short explanation, chosen by the first rule
// that fits. The ids name the groups in the `-diff.txt` files and the
// rows of the README's breakdown, so they stay stable across
// regenerations; the prose describes both sides without ranking them.

const SPEC = "GLOB_SPEC";

// Reasons for a pattern globstar refuses, by `GlobError` kind.
const REJECTS = {
  UnterminatedClass: {
    title: "A `[` that never closes",
    text:
      `globstar reads classes by POSIX's rules (${SPEC} §8.5): a \`]\` first in the list is ` +
      "literal, and a class cannot hold `/`, so a `[` with no closing `]` after that is a " +
      "parse error. The library accepts the pattern, as literal text or with the class " +
      "crossing `/`; its answer is the row's comment.",
  },
  UnterminatedBrace: {
    title: "A `{` that never closes",
    text:
      `An unclosed \`{\` is a parse error (${SPEC} §6.1); the library keeps the brace as ` +
      "literal text.",
  },
  InvalidRange: {
    title: "A reversed class range",
    text:
      `\`[z-a]\` is refused (${SPEC} §8.4); the library accepts the class as empty or as ` +
      "literal text.",
  },
  TrailingBackslash: {
    title: "A pattern ending in `\\`",
    text:
      `A \`\\\` at the end escapes nothing and is refused (${SPEC} §4.1); the library drops it ` +
      "or keeps it as a literal backslash.",
  },
  EscapedSeparator: {
    title: "An escaped separator",
    text:
      `\`\\/\` is refused: a separator has no literal form (${SPEC} §4.2). The library reads it ` +
      "as a plain `/`.",
  },
  Empty: {
    title: "The empty pattern",
    text: `The empty pattern is refused (${SPEC} §3); the library compiles it and matches the empty path.`,
  },
  TooLong: {
    title: "A pattern over the length limit",
    text: `The pattern is longer than ${SPEC} §6.2 allows.`,
  },
  BraceNestingTooDeep: {
    title: "Braces nested past the limit",
    text: `The braces nest deeper than ${SPEC} §6.2 allows.`,
  },
};

const THEY_REJECT = {
  title: "The library refuses the pattern",
  text:
    "The library's parser rejects the pattern and globstar's does not; the row records " +
    "globstar's answer on the path shown (`-` when the upstream test gave none).",
};

// Reasons for two answers that disagree, in the order they are tried.
// `when(pattern, path, row)` says whether the rule fits.
const DISAGREE = [
  {
    id: "quoted-text",
    title: "Quotes in the pattern",
    when: (p) => /["']/.test(p),
    text:
      "picomatch and micromatch read text in quotes as literal and drop the quotes, as a " +
      `shell would. globstar has no quoting: a quote is a plain character (${SPEC} §2).`,
  },
  {
    id: "class-or-literal",
    title: "A class matching its own text",
    when: (p, path) => /\[[^\]]+\]/.test(p) && path.includes("["),
    text:
      "picomatch and micromatch let a bracket expression match either one of its characters " +
      `or its own literal text, so \`*[1]\` matches \`copy [1]\`. A class matches one character (${SPEC} §8).`,
  },
  {
    id: "regex-quantifier",
    title: "A `+` after a class, brace or group",
    when: (p) => /[\])}]\+/.test(p),
    text:
      "picomatch and micromatch read `[a]+`, `{a,b}+` and `(a)+` as regex repetition. `+` is a " +
      `literal character here (${SPEC} §2).`,
  },
  {
    id: "regex-syntax",
    title: "Regex syntax in the pattern",
    when: (p) => /[()|]/.test(p),
    text:
      "picomatch and micromatch read `(…)` as a group and `|` as alternation, as a regex " +
      `would. globstar has no such syntax: the characters are literal (${SPEC} §2).`,
  },
  {
    id: "wax-syntax",
    title: "wax's own syntax",
    when: (p) => /<[^>]*:[^>]*>|\(\?|\$/.test(p),
    text:
      "wax has repetitions (`<a:1,>`), flags (`(?i)`) and a lazy `*` spelled `$`; globstar has " +
      "none of them, so " +
      `the characters are literal (${SPEC} §2).`,
  },
  {
    id: "brace-range",
    title: "A `{a..z}` range",
    when: (p) => /\{[^{}]*\.\.[^{}]*\}/.test(p),
    text:
      "A `{1..3}` or `{a..c}` brace is a range to expand for the library. globstar has no " +
      `ranges: the brace holds one literal branch, so it is literal text (${SPEC} §6.1).`,
  },
  {
    id: "brace-empty-branch",
    title: "An empty brace branch",
    when: (p) => /\{,|,\}|,,/.test(p),
    text:
      `An empty branch is a branch (${SPEC} §6.1): \`foo{,.txt}\` matches \`foo\` and \`foo.txt\`. ` +
      "The library drops the empty branch, or reads the brace as literal text.",
  },
  {
    id: "brace-degenerate",
    title: "A brace with one branch",
    when: (p) => /\{[^{},]*\}/.test(p),
    text:
      `A brace needs two branches to be a brace (${SPEC} §6.1): \`{a}\` and \`{}\` are literal text ` +
      "here, and the library expands them.",
  },
  {
    id: "class-non-ascii",
    title: "A non-ASCII character in a class",
    when: (p) => /\[[^\]]*[\u0080-\uffff][^\]]*\]/.test(p),
    text:
      `A class is a set of bytes (${SPEC} §8.1, §14): \`[金銀]\` lists the bytes of the two ` +
      "characters and matches one byte, never a whole character. The library matches a character.",
  },
  {
    id: "non-ascii-case",
    title: "Case folding beyond ASCII",
    when: (p, path, row) => row.ci && /[\u0080-\uffff]/.test(p + path),
    text: `Case folding covers ASCII letters only (${SPEC} §13); the library folds Unicode.`,
  },
  {
    id: "dot-segment",
    title: "A `.` or `..` segment",
    when: (p, path) => /(^|\/)\.\.?(\/|$)/.test(path) || /(^|\/)\.\.?(\/|$)/.test(p),
    text:
      `\`.\` and \`..\` are plain names in a path and in a pattern (${SPEC} §3.1); normalize paths ` +
      "first. The library strips a leading `./`, or never lets a wildcard match `.` or `..`.",
  },
  {
    id: "trailing-globstar",
    title: "A trailing `/**` and the directory itself",
    when: (p) => /\/\*\*\/?$/.test(p) || /\/\*\*\/?[},]/.test(p),
    text:
      `\`a/**\` needs the \`a/\` prefix: it matches \`a/\` and everything below, not \`a\` itself ` +
      `(${SPEC} §9.5). The library matches \`a\` too.`,
  },
  {
    id: "globstar-in-brace",
    title: "A `**` inside braces",
    when: (p) => /\{[^{}]*\*\*[^{}]*\}/.test(p),
    text:
      `globstar decides whether a \`**\` in a brace is a globstar on the expansion (${SPEC} §10.1), ` +
      "as Bash would after expanding; the library decides on the branch alone.",
  },
  {
    id: "globstar-glued",
    title: "A `**` glued to other text",
    when: (p) => /[^/{,]\*\*|\*\*[^/},]/.test(p),
    text:
      `A \`**\` that does not own its segment is a plain \`*\` (${SPEC} §9.1). The library lets ` +
      "it cross `/` anyway, or refuses the pattern.",
  },
  {
    id: "negated-class-slash",
    title: "A negated class and a `/`",
    when: (p, path) => /\[[!^]/.test(p) && path.includes("/"),
    text:
      `A class never matches \`/\` (${SPEC} §8.2), negated or not. The library lets \`[!a]\` ` +
      "consume a separator.",
  },
  {
    id: "backslash",
    title: "A backslash in the pattern",
    when: (p) => p.includes("\\"),
    text:
      `\`\\X\` is the literal \`X\` for every \`X\` (${SPEC} §4.1), inside classes too, and a path is ` +
      "never unescaped. The library gives some escapes another meaning (`\\b`, `\\w`), keeps the " +
      "backslash, or unescapes the path.",
  },
  {
    id: "double-slash",
    title: "Two separators in a row",
    when: (p, path) => p.includes("//") || path.includes("//"),
    text:
      `\`//\` is two boundaries with an empty name between them (${SPEC} §14.2); a pattern \`/\` ` +
      "matches one separator. The library folds repeated separators.",
  },
  {
    id: "trailing-slash",
    title: "A trailing `/`",
    when: (p, path) => path.endsWith("/") || p.endsWith("/"),
    text:
      `A trailing \`/\` ends in an empty name: \`a/*\` matches \`a/\`, \`a\` does not match \`a/\` ` +
      `(${SPEC} §9.5, §14.2). The library ignores a trailing \`/\`, or needs one on both sides.`,
  },
  {
    id: "dotfile",
    title: "A name starting with `.`",
    when: (p, path) => /(^|\/)\.[^/]/.test(path),
    text:
      `Under \`dot=false\` no wildcard starts a name with \`.\` (${SPEC} §3.1); under \`dot=true\` ` +
      "any does. The library draws this line elsewhere for some wildcards and positions.",
  },
  {
    id: "negation",
    title: "A leading `!`",
    when: (p) => p.startsWith("!"),
    text:
      `A leading \`!\` negates the whole pattern (${SPEC} §11); an even number of them cancels. ` +
      "The library negates differently, or not at all in this call.",
  },
  {
    id: "absolute",
    title: "A leading `/`",
    when: (p, path) => p.startsWith("/") || path.startsWith("/"),
    text:
      `A leading \`/\` is a literal root boundary (${SPEC} §12.1); \`**/x\` also matches \`/x\`. ` +
      "The library treats a rooted pattern or path differently.",
  },
  {
    id: "empty",
    title: "An empty pattern or path",
    when: (p, path) => p === "" || path === "",
    text: `The empty path is a path with no segment (${SPEC} §3); it matches only a pattern that accepts nothing.`,
  },
  {
    id: "other",
    title: "Other",
    when: () => true,
    text: "No rule above fits; read the rows.",
  },
];

// The reason for a row whose `ours` and `theirs` differ. `ours` is a
// token (`match`, `no-match`, `err:Kind`); `theirs` is a token or
// `{ err }`.
export function reasonFor(row, ours, theirs) {
  if (ours.startsWith("err:")) {
    const kind = ours.slice(4);
    const r = REJECTS[kind] ?? { title: `Refused: ${kind}`, text: "" };
    return { id: `we-reject-${kind}`, kind: "we-reject", ...r };
  }
  if (theirs.err !== undefined) return { id: "they-reject", kind: "they-reject", ...THEY_REJECT };
  const rule = DISAGREE.find((r) => r.when(row.pattern, row.path, row));
  return { id: rule.id, kind: "disagree", title: rule.title, text: rule.text };
}
