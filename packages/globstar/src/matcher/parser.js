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

// Sequence context: whether we're inside a brace, plus — for the
// globstar segment-ownership test (§8.1) — the brace's *expanded-form*
// neighbors: `{A,B}` is the union of the patterns with the brace
// replaced by each branch (§7), so a `**` at a branch edge is judged
// against what sits outside the brace.
const CTX_TOP = Object.freeze({ brace: false, prevBoundary: true, nextBoundary: true });

// Is the point before the next atom a segment boundary in the expanded
// form? Start-of-pattern and after-`/` are boundaries; a branch start
// inherits from outside the `{` — so `a{**,x}b` degrades its `**` while
// `{**,x}/b` keeps a real globstar. Judged on parsed `nodes` rather
// than raw bytes so escape sequences are handled correctly.
function boundaryBefore(nodes, ctx) {
  if (nodes.length === 0) return ctx.prevBoundary;
  return nodes[nodes.length - 1].tag === N_SEPARATOR;
}

// Mirror of `boundaryBefore` for the byte after an atom: pattern end
// (`undefined`) and `/` are boundaries; a branch end (`,` / `}`)
// inherits from outside the `}`. `undefined` also covers unterminated
// braces, whose errors surface later — the value is then a don't-care.
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

  const state = { input: bytes, pos: 0, brace_depth: 0, maybe_sep_distribution: false };

  // Leading `!` flips the result on each. Parity decides final negation.
  let negationCount = 0;
  while (state.pos < bytes.length && bytes[state.pos] === BANG) {
    negationCount++;
    state.pos++;
  }

  const body = parseSequence(state, CTX_TOP);
  return {
    body,
    isNegated: (negationCount & 1) === 1,
    // Parser hint: a `**` was parsed inside `{}`; false proves the
    // lowering's separator-distribution check cannot trigger (§7).
    maybeSepDistribution: state.maybe_sep_distribution,
  };
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

  const inBrace = ctx.brace;
  while (state.pos < input.length) {
    const b = input[state.pos];

    // Brace context stops at the branch separator (`,`) or closer (`}`).
    if (inBrace && (b === COMMA || b === RBRACE)) break;

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
      case LBRACE: {
        flushLit();
        // Expanded-form neighbors for branch-edge globstar ownership
        // (§7 expansion equation / §8.1).
        const prevBoundary = boundaryBefore(nodes, ctx);
        const nextBoundary = braceNextBoundary(state, ctx);
        parseBraceInto(state, nodes, prevBoundary, nextBoundary);
        break;
      }
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

function parseStar(state, nodes, ctx) {
  const { input } = state;
  // `**` is a globstar only when both sides are segment boundaries in
  // the EXPANDED form (§8.1, §7 equation) — see boundaryBefore/After.
  if (
    input[state.pos + 1] === STAR &&
    boundaryBefore(nodes, ctx) &&
    boundaryAfter(input[state.pos + 2], ctx)
  ) {
    if (state.brace_depth > 0) state.maybe_sep_distribution = true;
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
    return;
  }

  // Single `*`, or degenerate `**` mid-segment — the second `*` is
  // consumed next iteration and folds into one Star.
  nodes.push(star());
  state.pos++;
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
function parseBraceInto(state, nodes, prevBoundary, nextBoundary) {
  const branches = parseBrace(state, prevBoundary, nextBoundary);
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

// Structural class skip: `[!`/`[^`, POSIX first-`]` literal, `\` escapes;
// stops after `]` or at `/` (which the real parser rejects later anyway).
function skipClassCandidate(input, start) {
  let i = start;
  if (input[i] === BANG || input[i] === CARET) i++;
  if (input[i] === RBRACK) i++;
  while (i < input.length && input[i] !== RBRACK && input[i] !== SLASH) {
    if (input[i] === BACKSLASH) i++;
    i++;
  }
  return Math.min(i + 1, input.length);
}

// Scan ahead from the current `{` to its matching `}` (honoring
// escapes, class scopes, and nesting) and report whether the byte
// after it is a boundary in the expanded form. Read-only — malformed
// tails error out in the real parse, so their value is a don't-care.
function braceNextBoundary(state, ctx) {
  const { input } = state;
  let i = state.pos + 1;
  let depth = 0;
  while (i < input.length) {
    const b = input[i];
    if (b === BACKSLASH) {
      i = Math.min(i + 2, input.length);
    } else if (b === LBRACK) {
      i = skipClassCandidate(input, i + 1);
    } else if (b === LBRACE) {
      depth++;
      i++;
    } else if (b === RBRACE) {
      if (depth === 0) return boundaryAfter(input[i + 1], ctx);
      depth--;
      i++;
    } else {
      i++;
    }
  }
  return true; // unterminated — errors in the real parse
}

function parseBrace(state, prevBoundary, nextBoundary) {
  const { input } = state;
  const startPos = state.pos;
  state.pos++; // consume '{'
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
