# Decision: one total segment engine, no Pike VM

Date: 2026-10-12. Status: implemented in both runtimes, committed as
`35bdc8c` (2026-10-13) with the follow-up `6bed824`.

## The question

Can the segment-structured matcher (SSM, `07-segment-matcher.md`) take
every pattern, so the byte-level Pike VM fallback can go?

Before this change the SSM refused a pattern in four ways and the Pike
VM took it: more than 64 fork sequences from separator-crossing
braces, more than 64 NFA states in a sequence, more than 64 states in
an in-segment NFA (32 in JS), and two op shapes it had no element for
(`LeadingSeps` inside a brace branch, a bare `**` after a `**/` in
another branch). The Pike VM was slow where it ran (a 32-name
`dir<i>/**` union: 5× slower to match, 5× more memory) and was the
reason a union of a few dozen directory names fell off the fast path.

## What was done

1. **Graph, not forks.** The compile walks the ops keeping one *group*
   per distinct (boundary, partial segment) reaching a point of the
   pattern (`compile.rs::Builder::walk`). A separator-crossing brace
   clones the groups into each branch and merges the branch results by
   (boundary, partial segment), so the continuation after the brace is
   built once and shared. `{a,b}/` × k is 2k+1 nodes. A `**/` is kept
   *pending* until the next op so a bare `**` right after it collapses
   it (`x/**/**` ≡ `x/**`), across branches too, which is what the old
   flat-fork rewrite did within one fork.
2. **Chains for `is_match`.** The paths through the graph are
   enumerated while there are at most 64 (`MAX_CHAINS`); each path is
   matched as before — anchored compares for ≤ 1 globstar, a small NFA
   over the path's own nodes for more — in pattern order. Past 64
   paths, `is_match` runs the NFA over the whole graph.
3. **Growable active sets.** The graph NFA and the in-segment NFA are
   generic over a bit set (`bits.rs`): a `u64` up to 64 states, a boxed
   word slice beyond (JS: int32 up to 31, `Int32Array` beyond). So a
   120-segment pattern or a 35-class segment compiles like any other.
4. **Name lists.** The suffix product behind `*.{a,b,…}` and factored
   unions of names grows from 16 to 1024 entries; past 8 the set is
   sorted by (length, bytes) and binary-searched per length
   (`AffixIndex`). Beyond 1024 the in-segment NFA holds the alternation
   as states, without multiplying it out.
5. **The two orphan shapes** became rules of the walk: `LeadingSeps`
   turns a strict boundary lenient for the `**/` that follows, and a
   literal that holds a separator byte (`\\` on Windows) simply never
   equals a segment.
6. **Pike VM removed** from both runtimes, with the Thompson builder.
   The forced-engine test lanes now force the graph NFA instead
   (`Glob::is_match_nfa`, JS `__engine: "nfa"`), the JS string-mode
   differential compares against forced byte mode (`__engine: "bytes"`),
   and the benchmark tables lost their Pike VM columns. The one test
   that pinned a Pike VM / SSM difference on the all-below bit is gone
   with the Pike VM; GLOB_SPEC §13.6 now lists that shape as not
   provable, full stop.

## What it cost and bought

Measured with a same-binary interleaved A/B (old crate snapshot vs new,
median of 15–21 rounds, Rust; fresh process per row, 9 rounds, JS):

- Single patterns: match and `match_dir` unchanged within the ±5–10%
  noise of the harness; compile +2…+8% on the smallest patterns (the
  group walk and the graph tables cost one more allocation on a few
  rows). Memory per matcher +40…60 B (the graph tables).
- Unions of linear members: `match_dir` 1.6–2× faster (one NFA over the
  shared graph instead of one per member); `is_match` unchanged.
- Everything the Pike VM used to take: `dir<i>/**` × 32 matches 4.7×
  faster, × 200 34× faster, with 5× less memory; a 12-rule ignore list
  plus 60 directories matches 4.6× faster.

## Rejected on the way

- Running the whole-graph NFA for `is_match` on every union: a union of
  linear members with many branch heads costs one predicate per head
  per segment, 1.5× the chains.
- Masking the whole-graph NFA to one chain for multi-globstar members:
  the graph of a big union is wide (> 64 states), so every step
  touched boxed word slices; 3× slower than the old per-fork u64 NFA.
  Each such chain now carries its own small NFA instead.
- A memoized recursive builder keyed by (position, boundary, buffer):
  equivalent sharing, but its recursion depth follows the pattern
  length and it needs hashing; the group walk is iterative over the
  ops and recurses only into brace nesting.

## Verification

- Old crate (Segment + Pike VM) vs new, Rust: 1.4M random structured
  patterns (≈22k of them on the Pike VM before), 43M `is_match` and
  `match_dir` checks, no difference — including the all-below bit.
- Old JS package vs new: 180k patterns, 5.4M checks, no difference.
- JS ↔ Rust fuzz (`fuzz.mjs`), `verify.mjs` on every corpus, the
  bounded-exhaustive `match_dir` properties, the string-mode
  differential, clippy and lint: all green.
