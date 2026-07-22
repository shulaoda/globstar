// Linear instruction stream lowered from the AST.
//
// Three consumers:
//   - `LiteralFacts.extract(ops)` — suffix-anchored prefilter
//   - `Thompson.compile(program)` — NFA / DFA build
//   - `computeStaticPrefixes(ops)` — walker root prefixes
//
// Globstar folding (per `decisions/ADR-003`):
//
//   | Source         | Folded op           | Regex analogue   |
//   |----------------|---------------------|------------------|
//   | `**/`  (lead)  | `OptSegmentsSlash`  | `(?:[^/]*/)*`    |
//   | `/**/` (mid)   | `SepRun, OSS`       | `/+(?:[^/]*/)*`  |
//   | `/**`  (trail) | `SlashAnything`     | `/.*`            |
//   | `**`   (alone) | `GlobstarAny`       | `.*`             |
//
// Brace expansion stays as a single `Alternation` op — no cartesian
// expansion — so memory + match cost are independent of variant count.

import {
  N_LITERAL,
  N_SEPARATOR,
  N_ANYCHAR,
  N_STAR,
  N_GLOBSTAR,
  N_CLASS,
  N_CONCAT,
  N_BRACE,
  classExpandedAsciiCi,
  sep,
  brace,
  concat,
} from "../ast.js";
import { LiteralFacts } from "./facts.js";

// Op kinds.
export const OP_LIT = 0; // { kind, bytes }
export const OP_ANYCHAR = 1;
export const OP_STAR = 2;
export const OP_CLASS = 3; // { kind, cls }
export const OP_SEP = 4; // strict — exactly one separator
export const OP_SEP_RUN = 5; // lenient — one or more separators
export const OP_GLOBSTAR = 6; // raw, must be folded out before NFA build
export const OP_OPT_SEGMENTS_SLASH = 7; // `(?:[^/]*/)*`
export const OP_SLASH_ANYTHING = 8; // `/.*`
export const OP_GLOBSTAR_ANY = 9; // `.*`
export const OP_LEADING_SEPS = 10; // zero-or-more leading separators
export const OP_ALTERNATION = 11; // { kind, branches }

// Zero-field ops are immutable singletons. The fold passes only ever
// REPLACE array slots — never mutate existing op objects — so sharing
// is safe.
const ANYCHAR_OP = Object.freeze({ kind: OP_ANYCHAR });
const STAR_OP = Object.freeze({ kind: OP_STAR });
const SEP_OP = Object.freeze({ kind: OP_SEP });
const SEP_RUN_OP = Object.freeze({ kind: OP_SEP_RUN });
const GLOBSTAR_OP = Object.freeze({ kind: OP_GLOBSTAR });
const OSS_OP = Object.freeze({ kind: OP_OPT_SEGMENTS_SLASH });
const SLASH_ANY_OP = Object.freeze({ kind: OP_SLASH_ANYTHING });
const GSTAR_ANY_OP = Object.freeze({ kind: OP_GLOBSTAR_ANY });
const LEADING_SEPS_OP = Object.freeze({ kind: OP_LEADING_SEPS });

// Returns an `OpProgram`-shaped object with the literal facts
// prefilter pre-extracted.
export function lower(node, caseInsensitive) {
  const ops = [];
  // §7 expansion equation: a separator flanking a globstar-edged
  // brace belongs to every expansion — distribute it into the
  // branches so `{**,x}/b` means `**/b ∪ x/b` and `a/{**/x,y}` gets
  // the same lenient `/**/` boundary as `a/**/x`. Cheap scan first:
  // virtually all patterns skip the rebuild.
  const root = needsSepDistribution(node) ? distributeSeps(node) : node;
  lowerInto(root, ops, caseInsensitive);
  foldGlobstars(ops);
  applyLeadingSepsAtStart(ops);
  const facts = LiteralFacts.extract(ops, !!caseInsensitive);
  return { ops, facts, caseInsensitive: !!caseInsensitive };
}

// Does any brace sit next to a separator while holding a branch-edge
// globstar? (Trigger for `distributeSeps`.)
function needsSepDistribution(node) {
  if (node.tag === N_CONCAT) {
    const cs = node.children;
    for (let i = 0; i < cs.length; i++) {
      const c = cs[i];
      if (c.tag === N_BRACE) {
        const prevSep = i > 0 && cs[i - 1].tag === N_SEPARATOR;
        const nextSep = i + 1 < cs.length && cs[i + 1].tag === N_SEPARATOR;
        if (
          (prevSep && c.branches.some(leadsGlobstar)) ||
          (nextSep && c.branches.some(trailsGlobstar))
        ) {
          return true;
        }
      }
      if (needsSepDistribution(c)) return true;
    }
    return false;
  }
  if (node.tag === N_BRACE) return node.branches.some(needsSepDistribution);
  return false;
}

function leadsGlobstar(node) {
  if (node.tag === N_GLOBSTAR) return true;
  if (node.tag === N_CONCAT) return node.children.length > 0 && leadsGlobstar(node.children[0]);
  if (node.tag === N_BRACE) return node.branches.some(leadsGlobstar);
  return false;
}

function trailsGlobstar(node) {
  if (node.tag === N_GLOBSTAR) return true;
  if (node.tag === N_CONCAT) {
    const cs = node.children;
    return cs.length > 0 && trailsGlobstar(cs[cs.length - 1]);
  }
  if (node.tag === N_BRACE) return node.branches.some(trailsGlobstar);
  return false;
}

// Push separators flanking globstar-edged braces into every branch
// (recursively — an absorbed separator next to a nested brace keeps
// sinking). A separator shared by two qualifying braces goes to the
// LEFT brace's tails (deterministic). Never mutates input nodes.
function distributeSeps(node) {
  if (node.tag === N_CONCAT) {
    const out = [];
    const cs = node.children;
    let i = 0;
    while (i < cs.length) {
      const c = cs[i];
      if (c.tag !== N_BRACE) {
        out.push(distributeSeps(c));
        i += 1;
        continue;
      }
      const absorbPrev =
        out.length > 0 && out[out.length - 1].tag === N_SEPARATOR && c.branches.some(leadsGlobstar);
      const absorbNext =
        i + 1 < cs.length && cs[i + 1].tag === N_SEPARATOR && c.branches.some(trailsGlobstar);
      if (!absorbPrev && !absorbNext) {
        out.push(distributeSeps(c));
        i += 1;
        continue;
      }
      if (absorbPrev) out.pop();
      const branches = c.branches.map((b) => {
        const seq = [];
        if (absorbPrev) seq.push(sep());
        seq.push(b);
        if (absorbNext) seq.push(sep());
        // Re-distribute: the absorbed separator may now flank a
        // nested globstar-edged brace.
        return distributeSeps(concat(seq));
      });
      out.push(brace(branches));
      i += absorbNext ? 2 : 1;
    }
    return concat(out);
  }
  if (node.tag === N_BRACE) return brace(node.branches.map(distributeSeps));
  return node;
}

function lowerInto(node, out, caseInsensitive) {
  switch (node.tag) {
    case N_LITERAL:
      pushOp(out, { kind: OP_LIT, bytes: node.bytes });
      return;
    case N_SEPARATOR:
      pushOp(out, SEP_OP);
      return;
    case N_ANYCHAR:
      pushOp(out, ANYCHAR_OP);
      return;
    case N_STAR:
      pushOp(out, STAR_OP);
      return;
    case N_GLOBSTAR:
      pushOp(out, GLOBSTAR_OP);
      return;
    case N_CLASS: {
      const cls = caseInsensitive ? classExpandedAsciiCi(node) : node;
      pushOp(out, { kind: OP_CLASS, cls });
      return;
    }
    case N_CONCAT:
      for (const c of node.children) lowerInto(c, out, caseInsensitive);
      return;
    case N_BRACE: {
      const altBranches = [];
      for (const branch of node.branches) {
        const branchOps = [];
        lowerInto(branch, branchOps, caseInsensitive);
        foldGlobstars(branchOps);
        altBranches.push(branchOps);
      }
      pushOp(out, { kind: OP_ALTERNATION, branches: altBranches });
      return;
    }
  }
}

// Invariant: lowered program never has two consecutive Lit ops —
// merge them on push.
function pushOp(out, op) {
  if (op.kind === OP_LIT && out.length > 0 && out[out.length - 1].kind === OP_LIT) {
    const prev = out[out.length - 1];
    const merged = new Uint8Array(prev.bytes.length + op.bytes.length);
    merged.set(prev.bytes, 0);
    merged.set(op.bytes, prev.bytes.length);
    out[out.length - 1] = { kind: OP_LIT, bytes: merged };
    return;
  }
  out.push(op);
}

// Three-pass fold of raw `Globstar` + adjacent `Sep` into the dedicated
// picomatch-style ops. Each pass uses a two-pointer write/read index
// over the supplied array — no intermediate allocation.
function foldGlobstars(ops) {
  // Pass 1 — `Globstar Sep` → `OSS` (lead and middle `**/`).
  let write = 0,
    read = 0;
  while (read < ops.length) {
    if (ops[read].kind === OP_GLOBSTAR && ops[read + 1]?.kind === OP_SEP) {
      ops[write++] = OSS_OP;
      read += 2;
    } else {
      if (read !== write) ops[write] = ops[read];
      read++;
      write++;
    }
  }
  ops.length = write;

  // Pass 1b — `Sep` before `OSS` is the explicit `/` of mid-`/**/`;
  // upgrade it to `SepRun` so `a//b` matches `a/**/b` (matches
  // picomatch / globset / wax leniency).
  for (let i = 0; i + 1 < ops.length; i++) {
    if (ops[i].kind === OP_SEP && ops[i + 1].kind === OP_OPT_SEGMENTS_SLASH) {
      ops[i] = SEP_RUN_OP;
    }
  }

  // Pass 2 — `Sep Globstar` → `SlashAnything` (trailing `/**`). Run
  // after pass 1 so OSS absorbers are already in place.
  write = 0;
  read = 0;
  while (read < ops.length) {
    if (ops[read].kind === OP_SEP && ops[read + 1]?.kind === OP_GLOBSTAR) {
      ops[write++] = SLASH_ANY_OP;
      read += 2;
    } else {
      if (read !== write) ops[write] = ops[read];
      read++;
      write++;
    }
  }
  ops.length = write;

  // Pass 3 — bare `Globstar` (standalone `**` or post-fold leftover) →
  // `GlobstarAny`.
  for (let i = 0; i < ops.length; i++) {
    if (ops[i].kind === OP_GLOBSTAR) ops[i] = GSTAR_ANY_OP;
  }
}

// Pattern-start `**/` (i.e. `OSS` as op[0]) means "X at any depth"
// (GLOB_SPEC §8.4) — covers relative paths, Unix absolute paths, and
// UNC paths. Prepending `LeadingSeps` lets the matcher consume any
// leading separator run uniformly.
//
// Recurses into top-level `Alternation` so `{**/a, README}` only
// prepends the marker on the `**/a` branch.
function applyLeadingSepsAtStart(ops) {
  if (ops.length === 0) return;
  const first = ops[0];
  if (first.kind === OP_OPT_SEGMENTS_SLASH) {
    ops.unshift(LEADING_SEPS_OP);
  } else if (first.kind === OP_ALTERNATION) {
    for (const b of first.branches) applyLeadingSepsAtStart(b);
  }
}

// Deepest segment-bounded literal prefix per program variant. `Alternation`
// at the head produces one prefix per branch.
//
//   `src/main.rs`    → "src/main.rs"  (fully literal)
//   `src/*.ts`       → "src"
//   `src/**/*.ts`    → "src"
//   `**/*.ts`        → ""
//   `{src,test}/*.rs`→ ["src", "test"]
export function computeStaticPrefixes(ops) {
  return dedupePrefixes(extractPrefixesPerBranch(ops));
}

function extractPrefixesPerBranch(ops) {
  if (ops.length > 0 && ops[0].kind === OP_ALTERNATION) {
    const out = [];
    for (const b of ops[0].branches) {
      for (const p of extractPrefixesPerBranch(b)) out.push(p);
    }
    return out;
  }
  return [extractLeadingPrefix(ops)];
}

function extractLeadingPrefix(ops) {
  const acc = [];
  let lastBoundary = 0;
  let fullyLiteral = true;
  for (const op of ops) {
    if (op.kind === OP_LIT) {
      for (let i = 0; i < op.bytes.length; i++) acc.push(op.bytes[i]);
    } else if (op.kind === OP_SEP || op.kind === OP_SEP_RUN) {
      acc.push(0x2f);
      lastBoundary = acc.length;
    } else {
      fullyLiteral = false;
      break;
    }
  }
  let len = fullyLiteral ? acc.length : lastBoundary;
  while (len > 0 && acc[len - 1] === 0x2f) len--;
  return Uint8Array.from(acc.slice(0, len));
}

// Drop entries covered by a shorter sibling (e.g. ["src", "src/cli"]
// → ["src"]). The empty prefix subsumes everything.
export function dedupePrefixes(prefixes) {
  prefixes.sort((a, b) => a.length - b.length);
  const result = [];
  for (const p of prefixes) {
    let covered = false;
    for (const r of result) {
      if (isDirPrefixOf(r, p)) {
        covered = true;
        break;
      }
    }
    if (!covered) result.push(p);
  }
  return result;
}

function isDirPrefixOf(short, long) {
  if (short.length === 0) return true;
  if (long.length < short.length) return false;
  for (let i = 0; i < short.length; i++) {
    if (long[i] !== short[i]) return false;
  }
  return long.length === short.length || long[short.length] === 0x2f;
}
