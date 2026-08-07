//! Abstract syntax tree for compiled glob patterns.
//!
//! Mirrors the BNF in GLOB_SPEC.md §3. The parser produces an [`Ast`] which
//! later passes (literal extraction, tier classification, NFA lowering) consume.
//!
//! The AST is byte-oriented — patterns are sequences of bytes, not UTF-8
//! characters. Multi-byte sequences are stored as-is in `Literal` nodes.

/// Root of a parsed pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ast {
    /// Outer negation count. Even = no negation, odd = negated.
    pub negation_count: u32,
    /// The pattern body.
    pub body: Node,
}

impl Ast {
    /// Whether the overall pattern is negated (odd `!` count).
    pub fn is_negated(&self) -> bool {
        self.negation_count % 2 == 1
    }
}

/// A single AST node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Concatenation of zero or more nodes.
    Concat(Vec<Node>),

    /// Literal bytes — no metacharacters; consecutive literals are merged.
    Literal(Vec<u8>),

    /// Path separator `/`.
    Separator,

    /// `?` — one non-separator byte.
    AnyChar,

    /// `*` — zero or more non-separator bytes.
    Star,

    /// `**` — zero or more bytes across separators; must own a whole segment.
    Globstar,

    /// `[...]` character class.
    Class(CharClass),

    /// `{a,b,c}` brace alternation.
    Brace(Vec<Node>),
}

/// A character class `[...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharClass {
    pub negated: bool,
    pub items: Vec<ClassItem>,
}

/// One element of a character class: a single byte or a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassItem {
    Byte(u8),
    Range(u8, u8),
}

impl ClassItem {
    pub fn matches(self, b: u8) -> bool {
        match self {
            Self::Byte(x) => x == b,
            Self::Range(lo, hi) => lo <= b && b <= hi,
        }
    }
}

impl CharClass {
    /// Whether byte `b` is in the class, honoring `negated`. Path
    /// separators are never members, either polarity (§6.2 / §12.3 —
    /// classes are segment-local), so the leading guard rejects them
    /// before the polarity flip. `/` is always a separator; `\` is one on
    /// Windows, so a positive `[\]` matches `\` on Unix but not Windows.
    pub fn matches(&self, b: u8) -> bool {
        if std::path::is_separator(b as char) {
            return false;
        }
        let listed = self.items.iter().any(|it| it.matches(b));
        listed ^ self.negated
    }

    /// The 2-item positive class a case-insensitive ASCII letter folds
    /// to — `{b, alt}` with `alt` the opposite-case byte — or `None` when
    /// `b` isn't a foldable letter (caller emits a plain byte match).
    pub(crate) fn ci_letter(b: u8) -> Option<Self> {
        let alt = crate::options::ascii_case_alt(b);
        (alt != b).then(|| CharClass {
            negated: false,
            items: vec![ClassItem::Byte(b), ClassItem::Byte(alt)],
        })
    }

    /// A copy of this class with ASCII case-alternates added, so `[A]`
    /// matches `A` and `a`, `[A-Z]` matches `[A-Za-z]`, etc. Non-letters
    /// and non-ASCII bytes are left as-is — ASCII-only by design
    /// (spec §11.3 / §12.5).
    pub fn expanded_ascii_case_insensitive(&self) -> Self {
        let mut items = Vec::with_capacity(self.items.len() * 2);
        for item in &self.items {
            items.push(*item);
            match *item {
                ClassItem::Byte(b) => {
                    let alt = crate::options::ascii_case_alt(b);
                    if alt != b {
                        items.push(ClassItem::Byte(alt));
                    }
                }
                ClassItem::Range(lo, hi) => {
                    // Pure-upper / pure-lower ranges fold to a symmetric
                    // range; mixed ones fall back to per-letter items.
                    if lo >= b'A' && hi <= b'Z' {
                        items.push(ClassItem::Range(lo | 0x20, hi | 0x20));
                    } else if lo >= b'a' && hi <= b'z' {
                        items.push(ClassItem::Range(lo & !0x20, hi & !0x20));
                    } else {
                        for b in lo..=hi {
                            let alt = crate::options::ascii_case_alt(b);
                            if alt != b {
                                items.push(ClassItem::Byte(alt));
                            }
                        }
                    }
                }
            }
        }
        Self {
            negated: self.negated,
            items,
        }
    }
}

impl Node {
    /// Whether this node tree contains a globstar.
    pub fn has_globstar(&self) -> bool {
        match self {
            Node::Globstar => true,
            Node::Concat(xs) | Node::Brace(xs) => xs.iter().any(Node::has_globstar),
            _ => false,
        }
    }

    /// Whether this node tree is a pure literal sequence
    /// (only `Literal` and `Separator`, no wildcards or alternations).
    pub fn is_pure_literal(&self) -> bool {
        match self {
            Node::Literal(_) | Node::Separator => true,
            Node::Concat(xs) => xs.iter().all(Node::is_pure_literal),
            _ => false,
        }
    }

    /// Render this node tree to its byte sequence if it is a pure literal.
    pub fn to_literal_bytes(&self) -> Option<Vec<u8>> {
        if !self.is_pure_literal() {
            return None;
        }
        let mut out = Vec::new();
        self.append_literal_bytes(&mut out);
        Some(out)
    }

    fn append_literal_bytes(&self, out: &mut Vec<u8>) {
        match self {
            Node::Literal(bytes) => out.extend_from_slice(bytes),
            Node::Separator => out.push(b'/'),
            Node::Concat(xs) => {
                for x in xs {
                    x.append_literal_bytes(out);
                }
            }
            _ => {}
        }
    }
}
