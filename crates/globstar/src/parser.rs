//! Recursive-descent parser for glob patterns.
//!
//! Converts a `&[u8]` pattern into an [`Ast`]. Implements the BNF in
//! GLOB_SPEC.md §3 with the byte-level conventions of §2.
//!
//! The parser is written by hand (no parser combinator library) for two
//! reasons: (1) the grammar is small and performance-sensitive, (2) error
//! reporting needs precise byte offsets.

use crate::ast::*;
use crate::error::*;

/// Parse a glob pattern into an AST.
pub fn parse(input: &[u8]) -> Result<Ast, GlobError> {
    if input.is_empty() {
        return Err(GlobError::Empty);
    }
    if input.len() > MAX_PATTERN_LEN {
        return Err(GlobError::TooLong {
            len: input.len(),
            max: MAX_PATTERN_LEN,
        });
    }

    let mut p = Parser {
        input,
        pos: 0,
        brace_depth: 0,
    };

    // Leading `!` are negation markers. Each one flips the result;
    // parity decides whether the compiled pattern is negated.
    let mut negation_count = 0u32;
    while p.pos < input.len() && input[p.pos] == b'!' {
        negation_count += 1;
        p.pos += 1;
    }

    let body = p.parse_sequence(SequenceContext::Top)?;
    Ok(Ast {
        negation_count,
        body,
    })
}

/// Stop tokens differ depending on where we're parsing, and — for
/// the globstar segment-ownership test (§8.1) — the context carries
/// the brace's *expanded-form* neighbors: `{A,B}` is defined as the
/// union of the patterns with the brace replaced by each branch
/// (GLOB_SPEC §7), so a `**` at a branch edge is judged against what
/// sits outside the brace, not against the branch edge itself.
#[derive(Clone, Copy)]
enum SequenceContext {
    /// Top-level: stop at end of input.
    Top,
    /// Inside `{...}`: stop at `,` or `}` at the matching depth.
    Brace {
        /// Expanded-form neighbor before the `{`: pattern start or a
        /// separator (chained through nested braces).
        prev_boundary: bool,
        /// Expanded-form neighbor after the matching `}`.
        next_boundary: bool,
    },
}

impl SequenceContext {
    /// Is the point before the next atom a segment boundary in the
    /// expanded form? Start-of-pattern and after-`/` are boundaries;
    /// a branch start inherits from outside the `{` — so `a{**,x}b`
    /// degrades its `**` while `{**,x}/b` keeps a real globstar.
    /// Judged on parsed `nodes` rather than raw bytes so escape
    /// sequences are handled correctly.
    fn boundary_before(self, last: Option<&Node>) -> bool {
        match last {
            None => matches!(self, Self::Top | Self::Brace { prev_boundary: true, .. }),
            Some(Node::Separator) => true,
            _ => false,
        }
    }

    /// Mirror of `boundary_before` for the byte after an atom: pattern
    /// end and `/` are boundaries; a branch end (`,` / `}`) inherits
    /// from outside the `}`. `None` also covers unterminated braces,
    /// whose errors surface later — the value is then a don't-care.
    fn boundary_after(self, next: Option<u8>) -> bool {
        match next {
            None | Some(b'/') => true,
            Some(b',') | Some(b'}') => {
                matches!(self, Self::Brace { next_boundary: true, .. })
            }
            _ => false,
        }
    }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    brace_depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.input.get(self.pos + offset).copied()
    }

    fn parse_sequence(&mut self, ctx: SequenceContext) -> Result<Node, GlobError> {
        let remaining = self.input.len() - self.pos;
        let mut nodes: Vec<Node> = Vec::with_capacity(remaining / 2 + 1);
        let mut lit_buf: Vec<u8> = Vec::with_capacity(remaining.min(32));

        while self.pos < self.input.len() {
            let b = self.input[self.pos];

            // Check stop conditions for our context.
            match ctx {
                SequenceContext::Top => {}
                SequenceContext::Brace { .. } => {
                    if b == b',' || b == b'}' {
                        break;
                    }
                }
            }

            match b {
                b'\\' => {
                    // Escape: \X produces literal X (lenient policy, GLOB_SPEC §9.1).
                    self.pos += 1;
                    if self.pos >= self.input.len() {
                        return Err(GlobError::TrailingBackslash);
                    }
                    lit_buf.push(self.input[self.pos]);
                    self.pos += 1;
                }
                b'/' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    nodes.push(Node::Separator);
                    self.pos += 1;
                    // No collapse: each `/` becomes its own `Sep`, so
                    // pattern `a//b` requires two separator bytes in
                    // the path (matches picomatch / globset / fast-glob).
                    // The `**`-boundary leniency lives in fold Pass 1b
                    // (`Sep` adjacent to `OSS` is upgraded to `SepRun`).
                }
                b'?' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    nodes.push(Node::AnyChar);
                    self.pos += 1;
                }
                b'*' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    self.parse_star(&mut nodes, ctx);
                }
                b'[' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    let class = self.parse_class()?;
                    nodes.push(Node::Class(class));
                }
                b'{' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    // Expanded-form neighbors for branch-edge globstar
                    // ownership (§7 expansion equation / §8.1).
                    let prev_boundary = ctx.boundary_before(nodes.last());
                    let next_boundary = self.brace_next_boundary(ctx);
                    // Single-branch `{a}` is treated as the literal `{a}`
                    // (GLOB_SPEC §7.4). `<[Node; 1]>::try_from` both checks
                    // the length AND moves the single element out in one step.
                    match <[Node; 1]>::try_from(self.parse_brace(prev_boundary, next_boundary)?) {
                        Ok([single]) => {
                            nodes.push(Node::Literal(b"{".to_vec()));
                            if let Some(bytes) = single.to_literal_bytes() {
                                nodes.push(Node::Literal(bytes));
                            } else {
                                // Single non-literal branch — keep the surrounding `{}`
                                // as literals (matches picomatch / fast-glob).
                                nodes.push(single);
                            }
                            nodes.push(Node::Literal(b"}".to_vec()));
                        }
                        Err(branches) => nodes.push(Node::Brace(branches)),
                    }
                }
                _ => {
                    // Anything else (including `@ + ! ( ) |` and stray
                    // `] }`) is a literal byte under the symmetric-loose
                    // rule §9.1: closers are meta only when paired with
                    // their opener.
                    lit_buf.push(b);
                    self.pos += 1;
                }
            }
        }

        flush_literal(&mut lit_buf, &mut nodes);

        // Single-node sequence → return the node directly; otherwise wrap
        // in `Concat`. `try_from` handles the len check + move in one step.
        Ok(match <[Node; 1]>::try_from(nodes) {
            Ok([single]) => single,
            Err(nodes) => Node::Concat(nodes),
        })
    }

    fn parse_star(&mut self, nodes: &mut Vec<Node>, ctx: SequenceContext) {
        debug_assert_eq!(self.input[self.pos], b'*');

        // `**` is a globstar only when both sides are segment boundaries
        // in the EXPANDED form (GLOB_SPEC §8.1, §7 equation) — see
        // `boundary_before` / `boundary_after`.
        if self.peek_at(1) == Some(b'*')
            && ctx.boundary_before(nodes.last())
            && ctx.boundary_after(self.peek_at(2))
        {
            nodes.push(Node::Globstar);
            self.pos += 2;
            // Collapse consecutive `/**/`.
            while self.pos + 3 <= self.input.len()
                && &self.input[self.pos..self.pos + 3] == b"/**"
                && (self.pos + 3 == self.input.len() || self.input[self.pos + 3] == b'/')
            {
                self.pos += 3;
            }
            return;
        }

        // Single `*`, or degenerate `**` mid-segment — the second `*`
        // is consumed next iteration and folds into one Star.
        nodes.push(Node::Star);
        self.pos += 1;
    }

    fn parse_class(&mut self) -> Result<CharClass, GlobError> {
        let start_pos = self.pos;
        debug_assert_eq!(self.input[self.pos], b'[');
        self.pos += 1;

        let negated = matches!(self.peek(), Some(b'!') | Some(b'^'));
        if negated {
            self.pos += 1;
        }

        let mut items: Vec<ClassItem> = Vec::new();

        // POSIX convention (GLOB_SPEC §6.5): when `]` appears as the FIRST
        // character inside `[…]` (or after `[!`/`[^`), treat it as a literal
        // member of the class rather than the closing bracket. Matches
        // bash / fnmatch / fast-glob / picomatch. `[]` therefore becomes
        // "literal `]` + no closer" → `UnterminatedClass`.
        if self.peek() == Some(b']') {
            items.push(ClassItem::Byte(b']'));
            self.pos += 1;
        }

        loop {
            let b = match self.peek() {
                Some(b) => b,
                None => return Err(GlobError::UnterminatedClass { at: start_pos }),
            };
            if b == b']' {
                self.pos += 1;
                return Ok(CharClass { negated, items });
            }
            // Raw `/` at class-body top means the class never closed inside
            // its segment (GLOB_SPEC §6.2) — semantically identical to EOF
            // from the class parser's POV. `\` at this position is an escape
            // prefix, so we defer its check to parse_class_byte (where the
            // *resolved* byte is inspected).
            if b == b'/' {
                return Err(GlobError::UnterminatedClass { at: start_pos });
            }

            // Parse one item, possibly a range `a-z`.
            let low = self.parse_class_byte(start_pos)?;

            // Range?
            if self.peek() == Some(b'-')
                && self.peek_at(1).is_some()
                && self.peek_at(1) != Some(b']')
            {
                self.pos += 1; // consume `-`
                let high = self.parse_class_byte(start_pos)?;
                if high < low {
                    return Err(GlobError::InvalidRange {
                        at: start_pos,
                        low,
                        high,
                    });
                }
                items.push(ClassItem::Range(low, high));
            } else {
                items.push(ClassItem::Byte(low));
            }
        }
    }

    fn parse_class_byte(&mut self, class_start: usize) -> Result<u8, GlobError> {
        let b = self
            .peek()
            .ok_or(GlobError::UnterminatedClass { at: class_start })?;
        let resolved = if b == b'\\' {
            self.pos += 1;
            let next = self.peek().ok_or(GlobError::TrailingBackslash)?;
            self.pos += 1;
            next
        } else {
            self.pos += 1;
            b
        };
        // Only `/` is a pattern-level segment separator. Encountering it
        // (either raw — caught earlier in the loop — or via `\/` escape
        // here) means the class never closed inside its segment (§6.2).
        // `\` is an escape character in pattern syntax, NOT a separator;
        // it may legitimately appear as a literal class member (`[\\b]` ≡
        // `[\, b]`). Runtime `CharClass::matches` short-circuits `\` for
        // path matching because §12.3 normalizes path-side `\` to `/`.
        if resolved == b'/' {
            return Err(GlobError::UnterminatedClass { at: class_start });
        }
        Ok(resolved)
    }

    /// Scan ahead from the current `{` to its matching `}` (honoring
    /// escapes, class scopes, and nesting) and report whether the
    /// byte after it is a boundary in the expanded form. Read-only —
    /// parse errors surface later through the real parse, so any
    /// malformed tail just yields a don't-care value.
    fn brace_next_boundary(&self, ctx: SequenceContext) -> bool {
        debug_assert_eq!(self.input[self.pos], b'{');
        let mut i = self.pos + 1;
        let mut depth = 0usize;
        while i < self.input.len() {
            match self.input[i] {
                b'\\' => i += 1,
                b'[' => {
                    // Class sub-scanner: `[!`/`[^` then POSIX first-`]`
                    // literal, `\` escapes; stops at `]` or a `/`
                    // (which the real parser rejects later anyway).
                    i += 1;
                    if matches!(self.input.get(i), Some(b'!') | Some(b'^')) {
                        i += 1;
                    }
                    if self.input.get(i) == Some(&b']') {
                        i += 1;
                    }
                    while i < self.input.len()
                        && self.input[i] != b']'
                        && self.input[i] != b'/'
                    {
                        if self.input[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                b'{' => depth += 1,
                b'}' => {
                    if depth == 0 {
                        return ctx.boundary_after(self.input.get(i + 1).copied());
                    }
                    depth -= 1;
                }
                _ => {}
            }
            i += 1;
        }
        // Unterminated brace — the real parse errors out; don't-care.
        true
    }

    fn parse_brace(
        &mut self,
        prev_boundary: bool,
        next_boundary: bool,
    ) -> Result<Vec<Node>, GlobError> {
        let start_pos = self.pos;
        debug_assert_eq!(self.input[self.pos], b'{');
        self.pos += 1;

        self.brace_depth += 1;
        if self.brace_depth > MAX_BRACE_NESTING {
            return Err(GlobError::BraceNestingTooDeep {
                max: MAX_BRACE_NESTING,
            });
        }

        let mut branches = Vec::new();
        loop {
            let branch = self.parse_sequence(SequenceContext::Brace {
                prev_boundary,
                next_boundary,
            })?;
            branches.push(branch);

            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    continue;
                }
                Some(b'}') => {
                    self.pos += 1;
                    self.brace_depth -= 1;
                    return Ok(branches);
                }
                _ => return Err(GlobError::UnterminatedBrace { at: start_pos }),
            }
        }
    }
}

fn flush_literal(buf: &mut Vec<u8>, nodes: &mut Vec<Node>) {
    if !buf.is_empty() {
        nodes.push(Node::Literal(std::mem::take(buf)));
    }
}
