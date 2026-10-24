# 01. The Parser and Unions

A pattern enters the library as text and leaves this stage as one AST, which the engine selection of §4 hands to one of two engines. A union of several patterns leaves it as one AST as well, its members factored into one body (§5). Source: `crates/globstar/src/parse/`, `union.rs` and `lib.rs`; JS `packages/globstar/src/parse/`, `union.js` and `glob.js`.

## 1. The AST

```
Node ::= Concat(Node*) | Literal(bytes) | Separator | AnyChar | Star | Globstar
       | Class(negated, ranges) | Brace(Node+)
Ast  ::= (body: Node, negated: bool)
```

A `Concat` holds two or more nodes; a sequence of one node is that node. A `Literal` holds the bytes of a run of literal characters, escapes resolved. A `Class` holds its byte ranges, a single byte as a range of one. A `Brace` holds one node per branch, an empty branch being an empty `Concat`. `Globstar` is a `**` that owns a segment (GLOB_SPEC §9.1); a `**` beside text is a `Star`.

## 2. Parsing

`parse/mod.rs::parse` is one left-to-right pass over the bytes of the pattern.

1. A pattern longer than 65,536 bytes is `TooLong`.
2. The leading run of `!` is counted; `Ast::negated` is its parity. A pattern that is nothing but `!` is `Empty` (GLOB_SPEC §4.3).
3. `parse_sequence` reads the body. Literal bytes accumulate in a buffer that is flushed as one `Literal` before any other node. `\` takes the next byte as a literal (`TrailingBackslash` at the end, `EscapedSeparator` before `/`). `/` is a `Separator`, `?` an `AnyChar`. A run of `*` is one `Star`, except a run of exactly two, which `double_star` decides (§2.1). `[` opens `parse_class`, `{` opens `parse_brace`.
4. Inside a brace, `,` ends a branch and `}` ends the brace; outside one, both are literal bytes (GLOB_SPEC §4.1).

`parse_class` reads a `!` or `^` after the `[` as negation, takes a `]` in first position as a member (GLOB_SPEC §8.5), reads `a-b` as a range (`InvalidRange` when `b < a`), resolves `\X` to the member `X`, and reports `UnterminatedClass` at a `/` or at the end of the input (GLOB_SPEC §8.2).

`parse_brace` reads branches with `parse_sequence` at depth one more; a depth past 32 is `BraceNestingTooDeep`, and an input that ends before `}` is `UnterminatedBrace`. A brace with one branch is not a brace (GLOB_SPEC §10.4): its `{`, the branch's bytes and its `}` go back into the enclosing literal buffer, and the branch's non-literal nodes, if any, are spliced in between.

Each scope is read to its own closer without regard to the enclosing text, which gives the readings of GLOB_SPEC §4.4.

### 2.1 Deciding a `**`

A `**` is a globstar when a separator or an edge of the pattern lies on both sides of it, and a star otherwise (GLOB_SPEC §9.1). The parser decides what it can see: a `**` with text before it (any node but a separator or a brace) or text after it (any byte but `/`, `{`, the end, or `,` and `}` inside a brace) is a `Star`. Every other `**` is a `Globstar` node, and the graph walk of the segment engine (note 03, §3.2) decides it again per brace expansion, since beside a brace the neighbor depends on the branch.

A `**/**`, where both are globstars in every expansion, is folded into one as it is read: the second `**` is dropped when the nodes so far end in `Globstar, Separator` with a separator or the pattern start before them, and a `/` or the end follows.

### 2.2 Negation

The leading `!` is read at the level of the predicate: `match(!P, w) = ¬match(P, w)`. The language of `P` is unchanged, and no automaton for the complement is built. (The bash extglob `!(p)` would require one, and its construction passes through a deterministic automaton of up to `2^{|p|}` states; the dialect therefore has no negation below the whole pattern.) `Glob::is_match` computes the body's answer and inverts it; `match_dir` of a negated pattern reports the exact `matches` and leaves both flags about "below" conservative (GLOB_SPEC §15.4).

## 3. Three empty things

- **The empty pattern.** The string of length zero, or one that is all `!`, is `Empty`.
- **The empty string ε.** A member of `L(*)`, `L(**)` and `L({a,})`.
- **The empty language ∅.** No pattern denotes it, except on Windows, where `\ ∈ Seps` and a literal or class `\` matches no separator: `\\` and `[\\]` match nothing (GLOB_SPEC §4.2, §8.2). The segment engine keeps such a segment as an element that takes no segment (note 03, §7), and the static prefixes of a dead expansion are none.

## 4. Engine selection

`lib.rs::Glob::from_body` chooses the engine by the body alone:

- a body whose nodes are literals and separators only (`Node::to_literal_bytes`) is the literal engine's (note 02), with its bytes joined by `/`;
- every other body is the segment engine's (note 03).

The two options, `dot` and `case_insensitive`, are parameters of the engine and are not consulted while parsing (GLOB_SPEC §3). A union whose members factor back into a literal (`["a", "\a"]`) takes the literal engine as well.

## 5. Unions

### 5.1 Members

`Glob::union_with` parses each member in turn. A member given twice is parsed once (a set of the texts, kept only for a list of more than one). A member that does not parse raises `InMember { index, error }`, with the index counting every member given; a negated member raises `NegatedInUnion`. No member leaves one body, and one leaves its own; several are factored by `union.rs::factor_branches` into one body, which the engine selection of §4 then treats as a pattern.

The JS `compileMatcher` accepts negated members: the positive members are factored and compiled as one engine, and each negated member becomes an engine of its own. The matcher's `match` is true when the positive engine matches or some negated engine does not; `matchDir` with negated members reports the exact match, `may_match_below` as true, and the positive engine's `matches_all_below` (GLOB_SPEC §16.2); `staticPrefixes` is the empty prefix.

### 5.2 Factoring

`factor_branches` turns the members' sequences into one sequence with one brace, so that the segment engine sees their common parts once:

1. **Lift tokens.** The whole tokens every member starts with are moved before the brace, and the tokens every member ends with after it (`lift_tokens`). A `**/`, `/**/` or `/**` is lifted whole or not at all (`group`).
2. **Take back.** When what is left of several members lies within one segment, the lifted tokens would make those members one in-segment brace. That is kept when the brace is a set (literal branches, no wildcard after them in the segment) or holds no literal at all (`?` with `??*`, whose in-segment NFA is exact). A mixed brace would be one in-segment NFA over all of them, so the tokens go back into the members and the members stay apart.
3. **Lift bytes.** When tokens were lifted in front, the bytes every member's first literal shares are lifted as one literal, and the same at the back (`lift_bytes`). Bytes are lifted only behind a lifted token, so the static prefixes of the members are kept: `{src/**,spec/**}` stays, `s{rc,pec}/**` would lose `src` and `spec`.
4. **Merge segments.** Members that differ only in the literal ending one segment become one member with a brace there (`merge_segments`, §5.3).
5. **Assemble.** Members that are left empty are one empty branch (a union of equal members is that member); one member is no brace; the result is the lifted head, the brace, and the lifted tail.

### 5.3 Merging segments

`merge_segments` takes the members made only of literals, separators and the wildcards `*`, `?` and `**`, with at least one wildcard in the member or among the lifted tokens, and more than one node. For each such member and each of its segments that ends in a literal, it records (segment index from the end, a cheap key, member, node). The cheap key is the member's node count and its first node's kind, length and edge bytes (its last node's when the first is the literal itself). The records are sorted by segment (deepest first), key and member, and each group of records alike is keyed again by a hash of the member with the literal replaced by a marker (`hash_without`); members in one hash run are compared node by node (`same_but`), and those that agree are merged into the first: its literal becomes a brace of all their literals, and the others are dropped. A member joins the deepest group it fits and is not keyed again at shallower segments.

The reads are budgeted: the sum of the merged members' lengths over all records may not exceed 16 per node of the union, and at least 4,096; a union past that is not merged. Real unions read each member once or twice; members alike in every segment but never equal would be read once per segment.

Examples: `**/a/**` with `**/b/**` gives `**/{a,b}/**`; `**/*.log` with `**/*.tmp` gives `**/*{.log,.tmp}`; `src/**/*.ts` with `tests/**/*.ts` gives `{src,tests}/**/*.ts`.

## 6. Complexity

Parsing is one pass, linear in the pattern. Factoring reads each member a bounded number of times: the lifts read the edges of every member once per token lifted, and merging is bounded by its budget. The result is linear in the members' total length.

## 7. Source map

| Symbol                                                            | File                        |
| ----------------------------------------------------------------- | --------------------------- |
| `parse`, `Parser::parse_sequence`, `double_star`, `parse_class`, `parse_brace` | `parse/mod.rs`   |
| `Node`, `CharClass`, `Ast`, `Node::to_literal_bytes`              | `parse/ast.rs`              |
| `Glob::new_with`, `Glob::union_with`, `Glob::from_body`           | `lib.rs`                    |
| `factor_branches`, `lift_tokens`, `lift_bytes`, `merge_segments`, `merge_runs`, `cheap_key`, `hash_without`, `same_but` | `union.rs` |
| `compileMatcher`, `buildEngine`, `makeMatcher`                    | `packages/globstar/src/glob.js` |
