import {
  ANYCHAR_NODE,
  GLOBSTAR_NODE,
  N_CONCAT,
  N_LITERAL,
  SEP_NODE,
  STAR_NODE,
  brace,
  concat,
} from "./parse/ast.js";

const FREE = 0;
const HEAD = 1;
const ABSORBED = 2;
const MASK = 1;

export function factorBranches(branches) {
  const seqs = branches.map((n) => (n.tag === N_CONCAT ? n.children.slice() : [n]));
  const members = mergeSegments(seqs).map((s) => (s.length === 1 ? s[0] : concat(s)));
  return members.length === 1 ? members[0] : brace(members);
}

function mergeSegments(seqs) {
  const entries = [];
  for (let i = 0; i < seqs.length; i++) {
    const s = seqs[i];
    const whole = memberHash(s);
    if (whole === null) continue;
    let k = 0;
    for (let at = 0; at < s.length; at++) {
      const n = s[at];
      if (n === SEP_NODE) k++;
      else if (n.tag === N_LITERAL && (at + 1 === s.length || s[at + 1] === SEP_NODE)) {
        entries.push([k, (whole - salted(litHash(n.str), at) + salted(MASK, at)) | 0, i, at]);
      }
    }
  }
  if (entries.length < 2) return seqs;
  const order = (x, y) => y[0] - x[0] || x[1] - y[1] || x[2] - y[2];
  if (entries.length > 8) entries.sort(order);
  else {
    for (let i = 1; i < entries.length; i++) {
      const e = entries[i];
      let j = i;
      for (; j > 0 && order(entries[j - 1], e) > 0; j--) entries[j] = entries[j - 1];
      entries[j] = e;
    }
  }
  const state = new Uint8Array(seqs.length);
  let absorbed = 0;
  for (let run = 0; run < entries.length; ) {
    const [k, hash] = entries[run];
    let end = run + 1;
    while (end < entries.length && entries[end][0] === k && entries[end][1] === hash) end++;
    let h = run;
    while (h < end && state[entries[h][2]] !== FREE) h++;
    if (h < end) {
      const [, , head, headAt] = entries[h];
      const lits = [];
      for (let e = h + 1; e < end; e++) {
        const [, , i, at] = entries[e];
        if (state[i] !== FREE || !sameBut(seqs[head], seqs[i], headAt)) continue;
        state[i] = ABSORBED;
        absorbed++;
        lits.push(seqs[i][at]);
      }
      if (lits.length > 0) {
        state[head] = HEAD;
        lits.unshift(seqs[head][headAt]);
        seqs[head][headAt] = brace(lits);
      }
    }
    run = end;
  }
  return absorbed === 0 ? seqs : seqs.filter((_, i) => state[i] !== ABSORBED);
}

function memberHash(s) {
  if (s.length < 2) return null;
  let wild = false;
  let hash = s.length;
  for (let at = 0; at < s.length; at++) {
    const n = s[at];
    let h;
    if (n === SEP_NODE) h = 3;
    else if (n === GLOBSTAR_NODE) {
      wild = true;
      h = 4;
    } else if (n === STAR_NODE) {
      wild = true;
      h = 5;
    } else if (n === ANYCHAR_NODE) {
      wild = true;
      h = 6;
    } else if (n.tag === N_LITERAL) h = litHash(n.str);
    else return null;
    hash = (hash + salted(h, at)) | 0;
  }
  return wild ? hash : null;
}

function litHash(str) {
  let h = 0x811c9dc5 ^ str.length;
  for (let j = 0; j < str.length; j++) h = Math.imul(h ^ str.charCodeAt(j), 0x01000193);
  return h;
}

function salted(hash, at) {
  const x = hash ^ Math.imul(at + 1, 0x9e3779b1);
  return Math.imul(x ^ (x >>> 15), 0x85ebca6b);
}

function sameBut(a, b, at) {
  if (a.length !== b.length || b[at].tag !== N_LITERAL) return false;
  for (let j = 0; j < a.length; j++) if (j !== at && !nodeEq(a[j], b[j])) return false;
  return true;
}

function nodeEq(a, b) {
  return a === b || (a.tag === N_LITERAL && b.tag === N_LITERAL && a.str === b.str);
}
