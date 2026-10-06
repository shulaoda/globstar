use crate::ast::*;
use crate::error::*;
use crate::resolve::resolve_globstars;

pub fn parse(input: &[u8]) -> Result<Ast, GlobError> {
    parse_within(input, &mut { MAX_EXPANSION })
}

/// [`parse`] drawing on a shared expansion `budget` (§7.7): the patterns of
/// one union may copy [`MAX_EXPANSION`] between them, not each.
#[inline]
pub(crate) fn parse_within(input: &[u8], budget: &mut usize) -> Result<Ast, GlobError> {
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
        pending: false,
    };

    let mut negation_count = 0u32;
    while p.pos < input.len() && input[p.pos] == b'!' {
        negation_count += 1;
        p.pos += 1;
    }

    let mut body = p.parse_sequence(false)?;
    if p.pending {
        body = resolve_globstars(body, budget)?;
    }
    Ok(Ast {
        negation_count,
        body,
    })
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    brace_depth: usize,
    /// A `**` was left to `resolve_globstars` (see [`Parser::double_star`]).
    pending: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.input.get(self.pos + offset).copied()
    }

    fn parse_sequence(&mut self, in_brace: bool) -> Result<Node, GlobError> {
        let remaining = self.input.len() - self.pos;
        let node_capacity = if in_brace {
            (remaining / 2 + 1).min(8)
        } else {
            remaining / 2 + 1
        };
        let mut nodes: Vec<Node> = Vec::with_capacity(node_capacity);
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
                    nodes.push(if run == 2 {
                        self.double_star(&nodes, in_brace)
                    } else {
                        Node::Star
                    });
                }
                b'[' => {
                    flush_literal(&mut lit_buf, &mut nodes);
                    let class = self.parse_class()?;
                    nodes.push(Node::Class(class));
                }
                b'{' => match <[Node; 1]>::try_from(self.parse_brace()?) {
                    Ok([single]) => {
                        lit_buf.push(b'{');
                        let inner = match single {
                            Node::Concat(inner) => inner,
                            node => vec![node],
                        };
                        for node in inner {
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

        flush_literal(&mut lit_buf, &mut nodes);

        Ok(match <[Node; 1]>::try_from(nodes) {
            Ok([single]) => single,
            Err(nodes) => Node::Concat(nodes),
        })
    }

    /// A `**` is a globstar with a separator, or the end of the pattern, on
    /// both sides, and a star beside any other token (§8.1). Beside a brace,
    /// or at the edge of a branch, what it meets depends on the branch, so
    /// such a `**` is left to `resolve_globstars`; so is one after `**/`,
    /// which `resolve_globstars` folds into it (§8.6).
    fn double_star(&mut self, nodes: &[Node], in_brace: bool) -> Node {
        let before = match nodes.last() {
            None if !in_brace => Some(true),
            None | Some(Node::Brace(_)) => None,
            Some(node) => Some(matches!(node, Node::Separator)),
        };
        let after = match self.peek() {
            None | Some(b'/') => Some(true),
            Some(b'{') => None,
            Some(b',' | b'}') if in_brace => None,
            Some(_) => Some(false),
        };
        match (before, after) {
            (Some(false), _) | (_, Some(false)) => Node::Star,
            (Some(true), Some(true)) if !matches!(nodes, [.., Node::Globstar, Node::Separator]) => {
                Node::Globstar
            }
            _ => {
                self.pending = true;
                Node::Globstar
            }
        }
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

        let mut branches = Vec::with_capacity(4);
        loop {
            branches.push(self.parse_sequence(true)?);

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
