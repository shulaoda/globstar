use crate::ast::*;
use crate::error::*;

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
    fn boundary_before(self, last: Option<&Node>) -> bool {
        match last {
            None => matches!(
                self,
                Self::Top
                    | Self::Brace {
                        prev_boundary: true,
                        ..
                    }
            ),
            Some(Node::Separator) => true,
            Some(node @ Node::Brace(_)) => node_trails_boundary(node),
            _ => false,
        }
    }

    fn boundary_after(self, next: Option<u8>) -> bool {
        match next {
            None | Some(b'/') => true,
            Some(b',') | Some(b'}') => {
                matches!(
                    self,
                    Self::Brace {
                        next_boundary: true,
                        ..
                    }
                )
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
        let node_capacity = match ctx {
            SequenceContext::Top => remaining / 2 + 1,
            SequenceContext::Brace { .. } => (remaining / 2 + 1).min(8),
        };
        let mut nodes: Vec<Node> = Vec::with_capacity(node_capacity);
        let mut lit_buf: Vec<u8> = Vec::with_capacity(remaining.min(32));

        let in_brace = matches!(ctx, SequenceContext::Brace { .. });
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
                    self.parse_star(&mut nodes, ctx);
                }
                b'[' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    let class = self.parse_class()?;
                    nodes.push(Node::Class(class));
                }
                b'{' => {
                    let (single, next_after_brace) = self.scan_brace(ctx);
                    let (prev_boundary, next_boundary) = if single {
                        (false, false)
                    } else {
                        (
                            lit_buf.is_empty() && ctx.boundary_before(nodes.last()),
                            next_after_brace,
                        )
                    };
                    match <[Node; 1]>::try_from(self.parse_brace(prev_boundary, next_boundary)?) {
                        Ok([single]) => {
                            lit_buf.push(b'{');
                            match single.to_literal_bytes() {
                                Some(bytes) if !bytes.contains(&b'/') => {
                                    lit_buf.extend_from_slice(&bytes);
                                }
                                _ => {
                                    flush_literal(&mut lit_buf, &mut nodes);
                                    nodes.push(single);
                                }
                            }
                            lit_buf.push(b'}');
                        }
                        Err(branches) => {
                            flush_literal(&mut lit_buf, &mut nodes);
                            nodes.push(Node::Brace(branches));
                        }
                    }
                }
                _ => {
                    lit_buf.push(b);
                    self.pos += 1;
                }
            }
        }

        flush_literal(&mut lit_buf, &mut nodes);

        Ok(match <[Node; 1]>::try_from(nodes) {
            Ok([single]) => single,
            Err(nodes) => Node::Concat(nodes),
        })
    }

    fn parse_star(&mut self, nodes: &mut Vec<Node>, ctx: SequenceContext) {
        debug_assert_eq!(self.input[self.pos], b'*');

        if self.peek_at(1) == Some(b'*')
            && ctx.boundary_before(nodes.last())
            && ctx.boundary_after(self.peek_at(2))
        {
            nodes.push(Node::Globstar);
            self.pos += 2;
            while self.pos + 3 <= self.input.len()
                && &self.input[self.pos..self.pos + 3] == b"/**"
                && (self.pos + 3 == self.input.len() || self.input[self.pos + 3] == b'/')
            {
                self.pos += 3;
            }
            return;
        }

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

        let mut items: Vec<ClassItem> = Vec::with_capacity(4);

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
            let low = self.parse_class_byte(start_pos)?;
            if self.peek() == Some(b'-') && self.peek_at(1) != Some(b']') {
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
        if resolved == b'/' {
            return Err(GlobError::UnterminatedClass { at: class_start });
        }
        Ok(resolved)
    }

    fn scan_brace(&self, ctx: SequenceContext) -> (bool, bool) {
        debug_assert_eq!(self.input[self.pos], b'{');
        let input = self.input;
        let mut i = self.pos + 1;
        let mut depth = 0usize;
        let mut single = true;
        while i < input.len() {
            match input[i] {
                b'\\' => i = (i + 2).min(input.len()),
                b'[' => {
                    i += 1;
                    if matches!(input.get(i), Some(b'!') | Some(b'^')) {
                        i += 1;
                    }
                    if input.get(i) == Some(&b']') {
                        i += 1;
                    }
                    while i < input.len() && input[i] != b']' && input[i] != b'/' {
                        if input[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i = (i + 1).min(input.len());
                }
                b'{' => {
                    depth += 1;
                    i += 1;
                }
                b',' if depth == 0 => {
                    single = false;
                    i += 1;
                }
                b'}' => {
                    if depth == 0 {
                        return (single, ctx.boundary_after(input.get(i + 1).copied()));
                    }
                    depth -= 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }
        (single, true)
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

        let mut branches = Vec::with_capacity(4);
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

fn node_trails_boundary(node: &Node) -> bool {
    match node {
        Node::Separator => true,
        Node::Concat(children) => children.last().is_some_and(node_trails_boundary),
        Node::Brace(branches) => !branches.is_empty() && branches.iter().all(node_trails_boundary),
        _ => false,
    }
}
