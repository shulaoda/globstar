pub mod ast;

use crate::error::*;

use ast::*;

pub fn parse(input: &[u8]) -> Result<Ast, GlobError> {
    if input.len() > MAX_PATTERN_LEN {
        return Err(GlobError::TooLong {
            len: input.len(),
            max: MAX_PATTERN_LEN,
        });
    }
    let negations = input.iter().take_while(|&&b| b == b'!').count();
    if negations == input.len() {
        return Err(GlobError::Empty);
    }
    let mut p = Parser {
        input,
        pos: negations,
    };
    Ok(Ast {
        body: p.parse_sequence(0)?,
        negated: negations % 2 == 1,
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

    fn parse_sequence(&mut self, depth: usize) -> Result<Node, GlobError> {
        let in_brace = depth > 0;
        let remaining = self.input.len() - self.pos;
        let mut lit_buf: Vec<u8> = Vec::with_capacity(remaining.min(32));
        let mut nodes: Vec<Node> = if in_brace {
            Vec::new()
        } else {
            Vec::with_capacity(remaining / 2 + 1)
        };

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
        let after_sep = matches!(
            nodes[..],
            [.., Node::Separator, Node::Globstar, Node::Separator]
        ) || !in_brace && matches!(nodes[..], [Node::Globstar, Node::Separator]);
        if matches!(next, None | Some(b'/')) && after_sep {
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
        let mut first = true;
        loop {
            if self.peek() == Some(b']') && !first {
                self.pos += 1;
                return Ok(CharClass { negated, ranges });
            }
            first = false;
            let range_at = self.pos;
            let low = self.parse_class_byte(start_pos)?;
            if self.peek() == Some(b'-') && self.input.get(self.pos + 1) != Some(&b']') {
                self.pos += 1;
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
