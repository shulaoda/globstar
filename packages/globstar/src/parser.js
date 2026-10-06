import {
  N_CONCAT,
  N_LITERAL,
  N_SEPARATOR,
  N_GLOBSTAR,
  N_BRACE,
  lit,
  sep,
  anyChar,
  star,
  globstar,
  klass,
  brace,
  concat,
  classItemByte,
  classItemRange,
} from "./ast.js";
import { GlobError, MAX_PATTERN_LEN, MAX_BRACE_NESTING, MAX_EXPANSION } from "./error.js";
import { resolveGlobstars } from "./resolve.js";
import { toBytes } from "./utf8.js";

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

// `budget` is the expansion budget (§7.7): the patterns of one matcher share
// it, so together they may copy `MAX_EXPANSION`, not each.
export function parse(input, budget = { left: MAX_EXPANSION }) {
  const bytes = toBytes(input);
  if (bytes.length === 0) throw new GlobError("Empty");
  if (bytes.length > MAX_PATTERN_LEN) {
    throw new GlobError("TooLong", { len: bytes.length, max: MAX_PATTERN_LEN });
  }

  // `pending`: a `**` was left to `resolveGlobstars` (see `doubleStar`).
  const state = { input: bytes, pos: 0, brace_depth: 0, pending: false };

  let negationCount = 0;
  while (state.pos < bytes.length && bytes[state.pos] === BANG) {
    negationCount++;
    state.pos++;
  }

  let body = parseSequence(state, false);
  if (state.pending) body = resolveGlobstars(body, budget);
  return { body, isNegated: (negationCount & 1) === 1 };
}

function parseSequence(state, inBrace) {
  const { input } = state;
  const nodes = [];
  const litBuf = [];

  function flushLit() {
    if (litBuf.length > 0) {
      nodes.push(lit(Uint8Array.from(litBuf)));
      litBuf.length = 0;
    }
  }

  while (state.pos < input.length) {
    const b = input[state.pos];

    if (inBrace && (b === COMMA || b === RBRACE)) break;

    switch (b) {
      case BACKSLASH: {
        state.pos++;
        if (state.pos >= input.length) throw new GlobError("TrailingBackslash");
        if (input[state.pos] === SLASH) {
          throw new GlobError("EscapedSeparator", { at: state.pos - 1 });
        }
        litBuf.push(input[state.pos]);
        state.pos++;
        break;
      }
      case SLASH:
        flushLit();
        nodes.push(sep());
        state.pos++;
        break;
      case QUESTION:
        flushLit();
        nodes.push(anyChar());
        state.pos++;
        break;
      case STAR: {
        flushLit();
        let run = 0;
        while (input[state.pos] === STAR) {
          run++;
          state.pos++;
        }
        // A run of exactly two is a `**`; any other run is a star.
        nodes.push(run === 2 ? doubleStar(state, nodes, inBrace) : star());
        break;
      }
      case LBRACK:
        flushLit();
        nodes.push(parseClass(state));
        break;
      case LBRACE:
        parseBraceInto(state, nodes, litBuf, flushLit);
        break;
      default:
        litBuf.push(b);
        state.pos++;
    }
  }

  flushLit();

  if (nodes.length === 1) return nodes[0];
  return concat(nodes);
}

// A `**` is a globstar with a separator, or the end of the pattern, on both
// sides, and a star beside any other token (§8.1). Beside a brace, or at the
// edge of a branch, what it meets depends on the branch, so such a `**` is
// left to `resolveGlobstars`; so is one after `**/`, which `resolveGlobstars`
// folds into it (§8.6).
function doubleStar(state, nodes, inBrace) {
  const last = nodes.length > 0 ? nodes[nodes.length - 1] : undefined;
  let before;
  if (last === undefined) before = inBrace ? undefined : true;
  else if (last.tag === N_BRACE) before = undefined;
  else before = last.tag === N_SEPARATOR;
  const next = state.input[state.pos];
  let after;
  if (next === undefined || next === SLASH) after = true;
  else if (next === LBRACE || (inBrace && (next === COMMA || next === RBRACE))) after = undefined;
  else after = false;
  if (before === false || after === false) return star();
  const folds =
    nodes.length >= 2 && last.tag === N_SEPARATOR && nodes[nodes.length - 2].tag === N_GLOBSTAR;
  if (before === undefined || after === undefined || folds) state.pending = true;
  return globstar();
}

function parseClass(state) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++;

  let negated = false;
  if (input[state.pos] === BANG || input[state.pos] === CARET) {
    negated = true;
    state.pos++;
  }

  const items = [];
  if (input[state.pos] === RBRACK) {
    items.push(classItemByte(RBRACK));
    state.pos++;
  }

  while (true) {
    if (state.pos >= input.length) throw new GlobError("UnterminatedClass", { at: startPos });
    const b = input[state.pos];
    if (b === RBRACK) {
      state.pos++;
      return klass(negated, items);
    }
    const lo = parseClassByte(state, startPos);
    if (input[state.pos] === DASH && input[state.pos + 1] !== RBRACK) {
      state.pos++;
      const hi = parseClassByte(state, startPos);
      if (hi < lo) throw new GlobError("InvalidRange", { at: startPos, low: lo, high: hi });
      items.push(classItemRange(lo, hi));
    } else {
      items.push(classItemByte(lo));
    }
  }
}

function parseClassByte(state, classStart) {
  const { input } = state;
  if (state.pos >= input.length) throw new GlobError("UnterminatedClass", { at: classStart });
  const b = input[state.pos];
  let resolved;
  if (b === BACKSLASH) {
    state.pos++;
    if (state.pos >= input.length) throw new GlobError("TrailingBackslash");
    resolved = input[state.pos];
    state.pos++;
  } else {
    resolved = b;
    state.pos++;
  }
  if (resolved === SLASH) throw new GlobError("UnterminatedClass", { at: classStart });
  return resolved;
}

function parseBraceInto(state, nodes, litBuf, flushLit) {
  const branches = parseBrace(state);
  if (branches.length === 1) {
    litBuf.push(LBRACE);
    const single = branches[0];
    for (const node of single.tag === N_CONCAT ? single.children : [single]) {
      if (node.tag === N_LITERAL) {
        for (let i = 0; i < node.bytes.length; i++) litBuf.push(node.bytes[i]);
      } else {
        flushLit();
        nodes.push(node);
      }
    }
    litBuf.push(RBRACE);
  } else {
    flushLit();
    nodes.push(brace(branches));
  }
}

function parseBrace(state) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++;
  state.brace_depth++;
  if (state.brace_depth > MAX_BRACE_NESTING) {
    throw new GlobError("BraceNestingTooDeep", { max: MAX_BRACE_NESTING });
  }
  const branches = [];
  while (true) {
    branches.push(parseSequence(state, true));
    const next = input[state.pos];
    if (next === COMMA) {
      state.pos++;
      continue;
    }
    if (next === RBRACE) {
      state.pos++;
      state.brace_depth--;
      return branches;
    }
    throw new GlobError("UnterminatedBrace", { at: startPos });
  }
}
