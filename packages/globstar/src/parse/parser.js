import {
  N_CONCAT,
  N_LITERAL,
  N_SEPARATOR,
  N_GLOBSTAR,
  N_BRACE,
  lit,
  SEP_NODE,
  ANYCHAR_NODE,
  STAR_NODE,
  GLOBSTAR_NODE,
  klass,
  brace,
  concat,
} from "./ast.js";
import { GlobError, MAX_PATTERN_LEN, MAX_BRACE_NESTING, MAX_EXPANSION } from "../error.js";
import { resolveGlobstars } from "./globstar.js";
import { utf8Latin1, utf8Length } from "../utf8.js";

const BACKSLASH = 0x5c;
const SLASH = 0x2f;
const QUESTION = 0x3f;
const STAR = 0x2a;
const LBRACK = 0x5b;
const RBRACK = 0x5d;
const LBRACE = 0x7b;
const RBRACE = 0x7d;
const COMMA = 0x2c;
const BANG = 0x21;
const CARET = 0x5e;
const DASH = 0x2d;
// The bytes `parseSequence` has a case for (`,` and `}` only in a brace):
// a run of any other bytes joins the text as one slice.
const SPECIAL = new Uint8Array(256);
for (const b of [BACKSLASH, SLASH, QUESTION, STAR, LBRACK, LBRACE, COMMA, RBRACE]) SPECIAL[b] = 1;

// `budget` is the expansion budget (§6.2): the patterns of one matcher share
// it, so together they may copy `MAX_EXPANSION`, not each.
//
// The pattern is read as its UTF-8 bytes, one char per byte (see
// `utf8Latin1`), and a literal keeps its bytes as such a string.
export function parse(input, budget = { left: MAX_EXPANSION }) {
  // UTF-8 is never shorter than the UTF-16 string, so a string past the
  // limit is refused before it is copied.
  if (input.length > MAX_PATTERN_LEN) {
    throw new GlobError("TooLong", { len: utf8Length(input), max: MAX_PATTERN_LEN });
  }
  const bytes = utf8Latin1(input);
  if (bytes.length === 0) throw new GlobError("Empty");
  if (bytes.length > MAX_PATTERN_LEN) {
    throw new GlobError("TooLong", { len: bytes.length, max: MAX_PATTERN_LEN });
  }

  // Past the end `charCodeAt` is NaN, which ends the run.
  let negations = 0;
  while (bytes.charCodeAt(negations) === BANG) negations++;
  // `!` and `!!` negate nothing (§6.1).
  if (negations === bytes.length) throw new GlobError("Empty");

  // `pending`: a `**` was left to `resolveGlobstars` (see `doubleStar`).
  const state = { input: bytes, pos: negations, pending: false };
  let body = parseSequence(state, 0);
  if (state.pending) body = resolveGlobstars(body, budget);
  // `ascii`: no byte past ASCII anywhere, so none in a literal or a class.
  return { body, isNegated: (negations & 1) === 1, ascii: bytes === input };
}

// The sequence up to the end, or (`depth` > 0, inside that many braces)
// up to the `,` or `}` that ends its branch.
function parseSequence(state, depth) {
  const inBrace = depth > 0;
  const { input } = state;
  const nodes = [];
  let text = "";
  const flush = () => {
    if (text.length > 0) {
      nodes.push(lit(text));
      text = "";
    }
  };

  while (state.pos < input.length) {
    const b = input.charCodeAt(state.pos);

    if (inBrace && (b === COMMA || b === RBRACE)) break;

    switch (b) {
      case BACKSLASH: {
        state.pos++;
        if (state.pos >= input.length) throw new GlobError("TrailingBackslash");
        if (input.charCodeAt(state.pos) === SLASH) {
          throw new GlobError("EscapedSeparator", { at: state.pos - 1 });
        }
        text += input[state.pos];
        state.pos++;
        break;
      }
      case SLASH:
        flush();
        nodes.push(SEP_NODE);
        state.pos++;
        break;
      case QUESTION:
        flush();
        nodes.push(ANYCHAR_NODE);
        state.pos++;
        break;
      case STAR: {
        flush();
        let run = 0;
        while (input.charCodeAt(state.pos) === STAR) {
          run++;
          state.pos++;
        }
        // A run of exactly two is a `**`; any other run is a star.
        nodes.push(run === 2 ? doubleStar(state, nodes, inBrace) : STAR_NODE);
        break;
      }
      case LBRACK:
        flush();
        nodes.push(parseClass(state));
        break;
      case LBRACE: {
        const branches = parseBrace(state, depth + 1);
        if (branches.length > 1) {
          flush();
          nodes.push(brace(branches));
          break;
        }
        // One branch is no brace (§10.4): its text, braces included.
        text += "{";
        const single = branches[0];
        for (const node of single.tag === N_CONCAT ? single.children : [single]) {
          if (node.tag === N_LITERAL) text += node.str;
          else {
            flush();
            nodes.push(node);
          }
        }
        text += "}";
        break;
      }
      default: {
        const start = state.pos;
        do state.pos++;
        while (state.pos < input.length && SPECIAL[input.charCodeAt(state.pos)] === 0);
        text += input.slice(start, state.pos);
      }
    }
  }

  // Most brace branches are one literal, which needs no node list.
  if (nodes.length === 0 && text.length > 0) return lit(text);
  flush();

  if (nodes.length === 1) return nodes[0];
  return concat(nodes);
}

// A `**` is a globstar with a separator, or the end of the pattern, on both
// sides, and a star beside any other token (§9.1). Beside a brace, or at the
// edge of a branch, what it meets depends on the branch, so such a `**` is
// left to `resolveGlobstars`; so is one after `**/`, which `resolveGlobstars`
// folds into it (§9.6).
function doubleStar(state, nodes, inBrace) {
  const last = nodes.length > 0 ? nodes[nodes.length - 1] : undefined;
  let before;
  if (last === undefined) before = inBrace ? undefined : true;
  else if (last.tag === N_BRACE) before = undefined;
  else before = last.tag === N_SEPARATOR;
  const next = state.pos < state.input.length ? state.input.charCodeAt(state.pos) : undefined;
  let after;
  if (next === undefined || next === SLASH) after = true;
  else if (next === LBRACE || (inBrace && (next === COMMA || next === RBRACE))) after = undefined;
  else after = false;
  if (before === false || after === false) return STAR_NODE;
  const folds =
    nodes.length >= 2 && last.tag === N_SEPARATOR && nodes[nodes.length - 2].tag === N_GLOBSTAR;
  if (before === undefined || after === undefined || folds) state.pending = true;
  return GLOBSTAR_NODE;
}

function parseClass(state) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++;

  let negated = false;
  const opener = input.charCodeAt(state.pos);
  if (opener === BANG || opener === CARET) {
    negated = true;
    state.pos++;
  }

  const ranges = [];
  // A `]` first is a member (§8.5), and like any member it can start a
  // range: `[]-a]` is `]..=a`.
  let first = true;
  while (true) {
    if (state.pos >= input.length) throw new GlobError("UnterminatedClass", { at: startPos });
    const b = input.charCodeAt(state.pos);
    if (b === RBRACK && !first) {
      state.pos++;
      return klass(negated, ranges);
    }
    first = false;
    const rangeAt = state.pos;
    const lo = parseClassByte(state, startPos);
    if (input.charCodeAt(state.pos) === DASH && input.charCodeAt(state.pos + 1) !== RBRACK) {
      state.pos++;
      const hi = parseClassByte(state, startPos);
      if (hi < lo) throw new GlobError("InvalidRange", { at: rangeAt, low: lo, high: hi });
      ranges.push({ lo, hi });
    } else {
      ranges.push({ lo, hi: lo });
    }
  }
}

function parseClassByte(state, classStart) {
  const { input } = state;
  if (state.pos >= input.length) throw new GlobError("UnterminatedClass", { at: classStart });
  let resolved = input.charCodeAt(state.pos++);
  if (resolved === BACKSLASH) {
    if (state.pos >= input.length) throw new GlobError("TrailingBackslash");
    resolved = input.charCodeAt(state.pos++);
  }
  if (resolved === SLASH) throw new GlobError("UnterminatedClass", { at: classStart });
  return resolved;
}

// The brace at `pos`, the `depth`th one open.
function parseBrace(state, depth) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++;
  if (depth > MAX_BRACE_NESTING) {
    throw new GlobError("BraceNestingTooDeep", { max: MAX_BRACE_NESTING });
  }
  const branches = [];
  while (true) {
    branches.push(parseSequence(state, depth));
    const next = input.charCodeAt(state.pos);
    if (next === COMMA) {
      state.pos++;
      continue;
    }
    if (next === RBRACE) {
      state.pos++;
      return branches;
    }
    throw new GlobError("UnterminatedBrace", { at: startPos });
  }
}
