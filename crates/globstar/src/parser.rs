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

/// Stop tokens differ depending on where we're parsing.
#[derive(Clone, Copy)]
enum SequenceContext {
    /// Top-level: stop at end of input.
    Top,
    /// Inside `{...}`: stop at `,` or `}` at the matching depth.
    Brace,
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
                SequenceContext::Brace => {
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
                    self.parse_star(&mut nodes, ctx)?;
                }
                b'[' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    let class = self.parse_class()?;
                    nodes.push(Node::Class(class));
                }
                b'{' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    // Single-branch `{a}` is treated as the literal `{a}`
                    // (GLOB_SPEC §7.4). `<[Node; 1]>::try_from` both checks
                    // the length AND moves the single element out in one step.
                    match <[Node; 1]>::try_from(self.parse_brace()?) {
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

    fn parse_star(&mut self, nodes: &mut Vec<Node>, ctx: SequenceContext) -> Result<(), GlobError> {
        debug_assert_eq!(self.input[self.pos], b'*');

        // Check for `**` (globstar).
        if self.peek_at(1) == Some(b'*') {
            // Globstar must occupy a full segment (GLOB_SPEC §8.1):
            // - preceded by a segment boundary
            // - followed by a segment boundary
            //
            // "Segment boundary" means `/`, the start/end of the current
            // sequence (pattern start, or the start/end of a brace branch
            // like in `{**/a,**/b}`). We detect start-of-sequence by
            // checking `nodes` (empty / last is Separator) rather than
            // raw bytes, which correctly handles escape sequences.
            let prev_ok = nodes.is_empty() || matches!(nodes.last(), Some(Node::Separator));
            let after_star_pos = self.pos + 2;
            let at_end = after_star_pos == self.input.len();
            let next_byte = self.input.get(after_star_pos).copied();
            let next_ok = at_end
                || next_byte == Some(b'/')
                || (matches!(ctx, SequenceContext::Brace)
                    && matches!(next_byte, Some(b'}' | b',')));

            if prev_ok && next_ok {
                // Real globstar.
                nodes.push(Node::Globstar);
                self.pos += 2;
                // Collapse consecutive `/**/`.
                while self.pos + 3 <= self.input.len()
                    && &self.input[self.pos..self.pos + 3] == b"/**"
                    && (self.pos + 3 == self.input.len() || self.input[self.pos + 3] == b'/')
                {
                    self.pos += 3;
                }
                return Ok(());
            } else {
                // Degenerate `**` — treat as a single `*` (the second `*` will be
                // consumed in the next loop iteration as another `Star`, but
                // they collapse semantically to one).
                nodes.push(Node::Star);
                self.pos += 1;
                return Ok(());
            }
        }

        nodes.push(Node::Star);
        self.pos += 1;
        Ok(())
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

    fn parse_brace(&mut self) -> Result<Vec<Node>, GlobError> {
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
            let branch = self.parse_sequence(SequenceContext::Brace)?;
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
