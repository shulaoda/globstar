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

export function factorBranches(branches) {
  const seqs = branches.map((n) => (n.tag === N_CONCAT ? n.children.slice() : [n]));
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
        entries.push([k, (whole - salted(litHash(n.str), at)) | 0, i, at]);
      }
    }
  }
  sortEntries(entries);
  const absorbed = new Uint8Array(seqs.length);
  for (let run = 0; run < entries.length; ) {
    const [k, hash] = entries[run];
    let end = run + 1;
    while (end < entries.length && entries[end][0] === k && entries[end][1] === hash) end++;
    let h = run;
    while (h < end && absorbed[entries[h][2]] === 1) h++;
    if (h < end) {
      const [, , head, at] = entries[h];
      const lits = [seqs[head][at]];
      for (let e = h + 1; e < end; e++) {
        const i = entries[e][2];
        if (absorbed[i] === 1 || !sameBut(seqs[head], seqs[i], at)) continue;
        absorbed[i] = 1;
        lits.push(seqs[i][at]);
      }
      if (lits.length > 1) seqs[head][at] = brace(lits);
    }
    run = end;
  }
  const members = [];
  for (let i = 0; i < seqs.length; i++) {
    if (absorbed[i] === 0) members.push(seqs[i].length === 1 ? seqs[i][0] : concat(seqs[i]));
  }
  return members.length === 1 ? members[0] : brace(members);
}

function sortEntries(entries) {
  const order = (x, y) => y[0] - x[0] || x[1] - y[1] || x[2] - y[2];
  if (entries.length > 8) return entries.sort(order);
  for (let i = 1; i < entries.length; i++) {
    const e = entries[i];
    let j = i;
    for (; j > 0 && order(entries[j - 1], e) > 0; j--) entries[j] = entries[j - 1];
    entries[j] = e;
  }
  return entries;
}

function memberHash(s) {
  let hash = 0;
  let wild = false;
  for (let at = 0; at < s.length; at++) {
    const n = s[at];
    let h;
    if (n === SEP_NODE) h = 1;
    else if (n === GLOBSTAR_NODE || n === STAR_NODE || n === ANYCHAR_NODE) {
      wild = true;
      h = n === GLOBSTAR_NODE ? 2 : n === STAR_NODE ? 3 : 4;
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
  for (let j = 0; j < a.length; j++) {
    if (j === at) continue;
    const x = a[j];
    const y = b[j];
    if (x !== y && !(x.tag === N_LITERAL && y.tag === N_LITERAL && x.str === y.str)) return false;
  }
  return true;
}
