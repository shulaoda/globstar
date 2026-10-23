//! Text → the AST of one pattern: the parser, with `ast` as its output.

pub mod ast;

use crate::error::*;

use ast::*;

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

    let negations = input.iter().take_while(|&&b| b == b'!').count();
    // `!` and `!!` negate nothing (§6.1).
    if negations == input.len() {
        return Err(GlobError::Empty);
    }

    let mut p = Parser {
        input,
        pos: negations,
    };
    let body = p.parse_sequence(0)?;
    Ok(Ast {
        negated: negations % 2 == 1,
        body,
    })
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.input.get(self.pos + offset).copied()
    }

    /// The sequence up to the end, or (`depth` > 0, inside that many
    /// braces) up to the `,` or `}` that ends its branch.
    fn parse_sequence(&mut self, depth: usize) -> Result<Node, GlobError> {
        let in_brace = depth > 0;
        let remaining = self.input.len() - self.pos;
        // Most brace branches are one literal, which needs no node list.
        let mut nodes: Vec<Node> = if in_brace {
            Vec::new()
        } else {
            Vec::with_capacity(remaining / 2 + 1)
        };
        let mut lit_buf: Vec<u8> = Vec::with_capacity(remaining.min(32));

        while self.pos < self.input.len() {
            let b = self.input[self.pos];
            if in_brace && (b == b',' || b == b'}') {
                break;
            }
            match b {
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.input.len() {
                        return Err(GlobError::TrailingBackslash);
                    }
                    if self.input[self.pos] == b'/' {
                        return Err(GlobError::EscapedSeparator { at: self.pos - 1 });
                    }
                    lit_buf.push(self.input[self.pos]);
                    self.pos += 1;
                }
                b'/' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    nodes.push(Node::Separator);
                    self.pos += 1;
                }
                b'?' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    nodes.push(Node::AnyChar);
                    self.pos += 1;
                }
                b'*' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    let run = self.input[self.pos..]
                        .iter()
                        .take_while(|&&b| b == b'*')
                        .count();
                    self.pos += run;
                    // A run of exactly two is a `**`; any other run is a star.
                    if run == 2 {
                        self.double_star(&mut nodes, in_brace);
                    } else {
                        nodes.push(Node::Star);
                    }
                }
                b'[' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    let class = self.parse_class()?;
                    nodes.push(Node::Class(class));
                }
                b'{' => match <[Node; 1]>::try_from(self.parse_brace(depth + 1)?) {
                    Ok([single]) => {
                        lit_buf.push(b'{');
                        for node in single.into_seq() {
                            match node {
                                Node::Literal(bytes) => lit_buf.extend_from_slice(&bytes),
                                node => {
                                    flush_literal(&mut lit_buf, &mut nodes);
                                    nodes.push(node);
                                }
                            }
                        }
                        lit_buf.push(b'}');
                    }
                    Err(branches) => {
                        flush_literal(&mut lit_buf, &mut nodes);
                        nodes.push(Node::Brace(branches));
                    }
                },
                _ => {
                    lit_buf.push(b);
                    self.pos += 1;
                }
            }
        }

        if nodes.is_empty() && !lit_buf.is_empty() {
            return Ok(Node::Literal(lit_buf));
        }
        flush_literal(&mut lit_buf, &mut nodes);
        Ok(Node::of_seq(nodes))
    }

    /// A `**` is a globstar with a separator, or the edge of the pattern, on
    /// both sides, and a star beside any other token (§9.1): read beside
    /// text, it is a [`Node::Star`]. Beside a brace, or at the edge of a
    /// branch, what it meets depends on the expansion, and the graph walk
    /// decides it in each. A `**/**` that is two globstars in every
    /// expansion is one (§9.6).
    fn double_star(&self, nodes: &mut Vec<Node>, in_brace: bool) {
        let text_before = nodes
            .last()
            .is_some_and(|n| !matches!(n, Node::Separator | Node::Brace(_)));
        let next = self.peek();
        let text_after = match next {
            None | Some(b'/' | b'{') => false,
            Some(b',' | b'}') => !in_brace,
            Some(_) => true,
        };
        if text_before || text_after {
            nodes.push(Node::Star);
            return;
        }
        let surely_sep_before = |at: usize| match at.checked_sub(1) {
            None => !in_brace,
            Some(i) => matches!(nodes[i], Node::Separator),
        };
        let n = nodes.len();
        if matches!(next, None | Some(b'/'))
            && matches!(nodes[..], [.., Node::Globstar, Node::Separator])
            && surely_sep_before(n - 2)
        {
            nodes.pop();
            return;
        }
        nodes.push(Node::Globstar);
    }

    fn parse_class(&mut self) -> Result<CharClass, GlobError> {
        let start_pos = self.pos;
        debug_assert_eq!(self.input[self.pos], b'[');
        self.pos += 1;

        let negated = matches!(self.peek(), Some(b'!') | Some(b'^'));
        if negated {
            self.pos += 1;
        }

        let mut ranges = Vec::with_capacity(4);

        // A `]` first is a member (§8.5), and like any member it can start
        // a range: `[]-a]` is `]..=a`.
        let mut first = true;
        loop {
            let b = match self.peek() {
                Some(b) => b,
                None => return Err(GlobError::UnterminatedClass { at: start_pos }),
            };
            if b == b']' && !first {
                self.pos += 1;
                return Ok(CharClass { negated, ranges });
            }
            first = false;
            let range_at = self.pos;
            let low = self.parse_class_byte(start_pos)?;
            if self.peek() == Some(b'-') && self.peek_at(1) != Some(b']') {
                self.pos += 1; // consume `-`
                let high = self.parse_class_byte(start_pos)?;
                if high < low {
                    return Err(GlobError::InvalidRange {
                        at: range_at,
                        low,
                        high,
                    });
                }
                ranges.push((low, high));
            } else {
                ranges.push((low, low));
            }
        }
    }

    fn parse_class_byte(&mut self, class_start: usize) -> Result<u8, GlobError> {
        let b = self
            .peek()
            .ok_or(GlobError::UnterminatedClass { at: class_start })?;
        self.pos += 1;
        let resolved = if b == b'\\' {
            let next = self.peek().ok_or(GlobError::TrailingBackslash)?;
            self.pos += 1;
            next
        } else {
            b
        };
        if resolved == b'/' {
            return Err(GlobError::UnterminatedClass { at: class_start });
        }
        Ok(resolved)
    }

    /// The brace at `pos`, the `depth`th one open.
    fn parse_brace(&mut self, depth: usize) -> Result<Vec<Node>, GlobError> {
        let start_pos = self.pos;
        debug_assert_eq!(self.input[self.pos], b'{');
        self.pos += 1;

        if depth > MAX_BRACE_NESTING {
            return Err(GlobError::BraceNestingTooDeep {
                max: MAX_BRACE_NESTING,
            });
        }

        let mut branches = Vec::with_capacity(4);
        loop {
            branches.push(self.parse_sequence(depth)?);

            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
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
