import {
  N_SEPARATOR,
  N_BRACE,
  N_CONCAT,
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
  nodeToLiteralBytes,
} from "./ast.js";
import { GlobError, MAX_PATTERN_LEN, MAX_BRACE_NESTING } from "./error.js";
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

const CTX_TOP = Object.freeze({ brace: false, prevBoundary: true, nextBoundary: true });

function boundaryBefore(nodes, ctx) {
  if (nodes.length === 0) return ctx.prevBoundary;
  const last = nodes[nodes.length - 1];
  if (last.tag === N_SEPARATOR) return true;
  if (last.tag === N_BRACE) return nodeTrailsBoundary(last);
  return false;
}

function nodeTrailsBoundary(node) {
  if (node.tag === N_SEPARATOR) return true;
  if (node.tag === N_CONCAT) {
    return node.children.length > 0 && nodeTrailsBoundary(node.children[node.children.length - 1]);
  }
  if (node.tag === N_BRACE) {
    return node.branches.length > 0 && node.branches.every(nodeTrailsBoundary);
  }
  return false;
}

function boundaryAfter(next, ctx) {
  if (next === undefined || next === SLASH) return true;
  if (next === COMMA || next === RBRACE) return ctx.brace && ctx.nextBoundary;
  return false;
}

export function parse(input) {
  const bytes = toBytes(input);
  if (bytes.length === 0) throw new GlobError("Empty");
  if (bytes.length > MAX_PATTERN_LEN) {
    throw new GlobError("TooLong", { len: bytes.length, max: MAX_PATTERN_LEN });
  }

  const state = { input: bytes, pos: 0, brace_depth: 0 };

  let negationCount = 0;
  while (state.pos < bytes.length && bytes[state.pos] === BANG) {
    negationCount++;
    state.pos++;
  }

  const body = parseSequence(state, CTX_TOP);
  return {
    body,
    isNegated: (negationCount & 1) === 1,
  };
}

function parseSequence(state, ctx) {
  const { input } = state;
  const nodes = [];
  const litBuf = [];

  function flushLit() {
    if (litBuf.length > 0) {
      nodes.push(lit(Uint8Array.from(litBuf)));
      litBuf.length = 0;
    }
  }

  const inBrace = ctx.brace;
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
      case STAR:
        flushLit();
        parseStar(state, nodes, ctx);
        break;
      case LBRACK:
        flushLit();
        nodes.push(parseClass(state));
        break;
      case LBRACE: {
        const [single, nextAfterBrace] = scanBrace(state, ctx);
        const prevBoundary = single ? false : litBuf.length === 0 && boundaryBefore(nodes, ctx);
        const nextBoundary = single ? false : nextAfterBrace;
        parseBraceInto(state, nodes, litBuf, flushLit, prevBoundary, nextBoundary);
        break;
      }
      default:
        litBuf.push(b);
        state.pos++;
    }
  }

  flushLit();

  if (nodes.length === 1) return nodes[0];
  return concat(nodes);
}

function parseStar(state, nodes, ctx) {
  const { input } = state;
  if (
    input[state.pos + 1] === STAR &&
    boundaryBefore(nodes, ctx) &&
    boundaryAfter(input[state.pos + 2], ctx)
  ) {
    nodes.push(globstar());
    state.pos += 2;
    while (
      state.pos + 3 <= input.length &&
      input[state.pos] === SLASH &&
      input[state.pos + 1] === STAR &&
      input[state.pos + 2] === STAR &&
      (state.pos + 3 === input.length || input[state.pos + 3] === SLASH)
    ) {
      state.pos += 3;
    }
    return;
  }

  nodes.push(star());
  state.pos++;
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

function parseBraceInto(state, nodes, litBuf, flushLit, prevBoundary, nextBoundary) {
  const branches = parseBrace(state, prevBoundary, nextBoundary);
  if (branches.length === 1) {
    litBuf.push(LBRACE);
    const single = branches[0];
    const litBytes = nodeToLiteralBytes(single);
    if (litBytes !== null && !litBytes.includes(SLASH)) {
      for (let i = 0; i < litBytes.length; i++) litBuf.push(litBytes[i]);
    } else {
      flushLit();
      nodes.push(single);
    }
    litBuf.push(RBRACE);
  } else {
    flushLit();
    nodes.push(brace(branches));
  }
}

function scanBrace(state, ctx) {
  const { input } = state;
  let i = state.pos + 1;
  let depth = 0;
  let single = true;
  while (i < input.length) {
    const b = input[i];
    if (b === BACKSLASH) {
      i = Math.min(i + 2, input.length);
    } else if (b === LBRACK) {
      i++;
      if (input[i] === BANG || input[i] === CARET) i++;
      if (input[i] === RBRACK) i++;
      while (i < input.length && input[i] !== RBRACK && input[i] !== SLASH) {
        if (input[i] === BACKSLASH) i++;
        i++;
      }
      i = Math.min(i + 1, input.length);
    } else if (b === LBRACE) {
      depth++;
      i++;
    } else if (b === COMMA && depth === 0) {
      single = false;
      i++;
    } else if (b === RBRACE) {
      if (depth === 0) return [single, boundaryAfter(input[i + 1], ctx)];
      depth--;
      i++;
    } else {
      i++;
    }
  }
  return [single, true];
}

function parseBrace(state, prevBoundary, nextBoundary) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++;
  state.brace_depth++;
  if (state.brace_depth > MAX_BRACE_NESTING) {
    throw new GlobError("BraceNestingTooDeep", { max: MAX_BRACE_NESTING });
  }
  const ctx = { brace: true, prevBoundary, nextBoundary };
  const branches = [];
  while (true) {
    branches.push(parseSequence(state, ctx));
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
