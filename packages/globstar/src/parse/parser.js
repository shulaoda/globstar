import {
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
  seq,
} from "./ast.js";
import { GlobError, MAX_PATTERN_LEN, MAX_BRACE_NESTING } from "../error.js";
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

const SPECIAL = new Uint8Array(256);
for (const b of [BACKSLASH, SLASH, QUESTION, STAR, LBRACK, LBRACE, COMMA, RBRACE]) SPECIAL[b] = 1;

export function parse(input) {
  if (input.length > MAX_PATTERN_LEN) {
    throw new GlobError("TooLong", { len: utf8Length(input), max: MAX_PATTERN_LEN });
  }
  const bytes = utf8Latin1(input);
  if (bytes.length > MAX_PATTERN_LEN) {
    throw new GlobError("TooLong", { len: bytes.length, max: MAX_PATTERN_LEN });
  }

  let negations = 0;
  while (bytes.charCodeAt(negations) === BANG) negations++;
  if (negations === bytes.length) throw new GlobError("Empty");

  const state = { input: bytes, pos: negations };
  const body = parseSequence(state, 0);
  return { body, isNegated: (negations & 1) === 1, ascii: bytes === input };
}

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
        if (run === 2) doubleStar(state, nodes, inBrace);
        else nodes.push(STAR_NODE);
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
        text += "{";
        for (const node of seq(branches[0])) {
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

  if (nodes.length === 0 && text.length > 0) return lit(text);
  flush();
  if (nodes.length === 1) return nodes[0];
  return concat(nodes);
}

function doubleStar(state, nodes, inBrace) {
  const n = nodes.length;
  const last = n > 0 ? nodes[n - 1].tag : -1;
  const textBefore = last !== -1 && last !== N_SEPARATOR && last !== N_BRACE;
  const next = state.input.charCodeAt(state.pos);
  const end = Number.isNaN(next) || next === SLASH;
  const textAfter = !end && next !== LBRACE && !(inBrace && (next === COMMA || next === RBRACE));
  if (textBefore || textAfter) {
    nodes.push(STAR_NODE);
    return;
  }
  const afterSep =
    last === N_SEPARATOR &&
    n >= 2 &&
    nodes[n - 2].tag === N_GLOBSTAR &&
    (n === 2 ? !inBrace : nodes[n - 3].tag === N_SEPARATOR);
  if (end && afterSep) {
    nodes.pop();
    return;
  }
  nodes.push(GLOBSTAR_NODE);
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
  let first = true;
  while (true) {
    if (input.charCodeAt(state.pos) === RBRACK && !first) {
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
