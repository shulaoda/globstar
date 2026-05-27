// Recursive-descent glob parser. Bytes → AST.
// Implements GLOB_SPEC.md §3 grammar with the byte-level conventions of §2.

import {
  N_SEPARATOR,
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

// Byte literals for branch tests in the hot scan loop.
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

const CTX_TOP = 0;
const CTX_BRACE = 1;

export function parse(input) {
  const bytes = toBytes(input);
  if (bytes.length === 0) throw new GlobError("Empty");
  if (bytes.length > MAX_PATTERN_LEN) {
    throw new GlobError("TooLong", { len: bytes.length, max: MAX_PATTERN_LEN });
  }

  const state = { input: bytes, pos: 0, brace_depth: 0 };

  // Leading `!` flips the result on each. Parity decides final negation.
  let negationCount = 0;
  while (state.pos < bytes.length && bytes[state.pos] === BANG) {
    negationCount++;
    state.pos++;
  }

  const body = parseSequence(state, CTX_TOP);
  return { body, isNegated: (negationCount & 1) === 1 };
}

function parseSequence(state, ctx) {
  const { input } = state;
  const nodes = [];
  let litBuf = [];

  function flushLit() {
    if (litBuf.length > 0) {
      nodes.push(lit(Uint8Array.from(litBuf)));
      litBuf = [];
    }
  }

  while (state.pos < input.length) {
    const b = input[state.pos];

    // CTX_BRACE stops at the brace's separator (`,`) or closer (`}`).
    if (ctx === CTX_BRACE && (b === COMMA || b === RBRACE)) break;

    switch (b) {
      case BACKSLASH: {
        // `\X` → literal X (lenient escape, GLOB_SPEC §9.1).
        state.pos++;
        if (state.pos >= input.length) throw new GlobError("TrailingBackslash");
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
      case LBRACE:
        flushLit();
        parseBraceInto(state, nodes);
        break;
      default:
        // Anything else (including `@ ! ( ) |` and stray `] }`) is literal —
        // closers are meta only when paired with their opener (§9.1).
        litBuf.push(b);
        state.pos++;
    }
  }

  flushLit();

  // Single-child sequences elide the Concat wrapper.
  if (nodes.length === 1) return nodes[0];
  return concat(nodes);
}

// `**` is a globstar only when surrounded by segment boundaries
// (GLOB_SPEC §8.1). Otherwise it degrades to a single `*` (the second
// `*` is re-consumed on the next loop iteration as another Star).
function parseStar(state, nodes, ctx) {
  const { input } = state;
  if (input[state.pos + 1] !== STAR) {
    nodes.push(star());
    state.pos++;
    return;
  }
  const prevOk = nodes.length === 0 || nodes[nodes.length - 1].tag === N_SEPARATOR;
  const after = state.pos + 2;
  const atEnd = after === input.length;
  const next = input[after];
  const nextOk =
    atEnd || next === SLASH || (ctx === CTX_BRACE && (next === RBRACE || next === COMMA));

  if (!(prevOk && nextOk)) {
    // Degenerate `**` mid-segment: emit as Star and let the loop handle
    // the second `*`.
    nodes.push(star());
    state.pos++;
    return;
  }
  nodes.push(globstar());
  state.pos += 2;
  // Collapse `/**/**` runs to a single globstar.
  while (
    state.pos + 3 <= input.length &&
    input[state.pos] === SLASH &&
    input[state.pos + 1] === STAR &&
    input[state.pos + 2] === STAR &&
    (state.pos + 3 === input.length || input[state.pos + 3] === SLASH)
  ) {
    state.pos += 3;
  }
}

function parseClass(state) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++; // consume '['

  let negated = false;
  if (input[state.pos] === BANG || input[state.pos] === CARET) {
    negated = true;
    state.pos++;
  }

  const items = [];
  // POSIX: a leading `]` (after `[` or `[!`/`[^`) is a literal `]`.
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
    // Raw `/` mid-class means the class never closed inside its segment
    // (§6.2). Same outcome whether reached by raw or `\/`.
    if (b === SLASH) throw new GlobError("UnterminatedClass", { at: startPos });

    const lo = parseClassByte(state, startPos);
    if (
      input[state.pos] === DASH &&
      state.pos + 1 < input.length &&
      input[state.pos + 1] !== RBRACK
    ) {
      state.pos++; // consume '-'
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

// Append the parsed brace's nodes onto `nodes`. Single-branch braces
// `{a}` revert to literal `{a}` (GLOB_SPEC §7.4 — matches picomatch /
// fast-glob / bash).
function parseBraceInto(state, nodes) {
  const branches = parseBrace(state);
  if (branches.length === 1) {
    nodes.push(lit(Uint8Array.from([LBRACE])));
    const single = branches[0];
    const litBytes = nodeToLiteralBytes(single);
    if (litBytes !== null) {
      nodes.push(lit(litBytes));
    } else {
      nodes.push(single);
    }
    nodes.push(lit(Uint8Array.from([RBRACE])));
  } else {
    nodes.push(brace(branches));
  }
}

function parseBrace(state) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++; // consume '{'
  state.brace_depth++;
  if (state.brace_depth > MAX_BRACE_NESTING) {
    throw new GlobError("BraceNestingTooDeep", { max: MAX_BRACE_NESTING });
  }
  const branches = [];
  while (true) {
    branches.push(parseSequence(state, CTX_BRACE));
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
