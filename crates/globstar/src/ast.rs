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
    /// Literal byte sequence (no metacharacters; merged across consecutive literal bytes).
    Literal(Vec<u8>),
    /// Path separator `/`.
    Separator,
    /// `?` — single non-separator byte.
    AnyChar,
    /// `*` — zero or more non-separator bytes.
    Star,
    /// `**` — zero or more bytes including separators (must occupy a full segment).
    Globstar,
    /// `[...]` character class.
    Class(CharClass),
    /// `{a,b,c}` brace expansion.
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
    /// Whether this item matches the given byte.
    pub fn matches(self, b: u8) -> bool {
        match self {
            Self::Byte(x) => x == b,
            Self::Range(lo, hi) => lo <= b && b <= hi,
        }
    }
}

impl CharClass {
    /// Whether this class matches the given byte (taking [`negated`] into account).
    ///
    /// §6.2 / §12.3: classes are **segment-local** — no member of the
    /// platform's `Seps` set (path separators) ever matches, regardless
    /// of class polarity. `/` is always in `Seps` (spec-forced); `\` is
    /// in `Seps` iff `std::path::is_separator` returns true (Windows).
    ///
    /// This guard uniformly protects both polarities:
    /// - Negated `[^abc]`: would otherwise sneak through via
    ///   `!any ^ true = true` — guard prevents that.
    /// - Positive `[\\]` / `[\\abc]`: on Unix `\ ∉ Seps`, so the class
    ///   happily matches a literal `\`. On Windows `\ ∈ Seps`, so the
    ///   class silently doesn't match `\` — a literal-`\` positive
    ///   class is fundamentally incompatible with segment-local on
    ///   Windows (same reason parser rejects `[/]` everywhere).
    pub fn matches(&self, b: u8) -> bool {
        if std::path::is_separator(b as char) {
            return false;
        }
        let listed = self.items.iter().any(|it| it.matches(b));
        listed ^ self.negated
    }

    /// Return a copy of this class with ASCII case-alternate members added
    /// so that `[A]` matches both `A` and `a`, `[A-Z]` matches `[A-Za-z]`,
    /// etc. Used by `ops::lower` when `case_insensitive` is set.
    ///
    /// Non-letter bytes are left unchanged; non-ASCII bytes (≥ 0x80) are
    /// not case-folded — ASCII-only by design (spec §11.3 / §12.5).
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
                    // Pure-upper range → add the symmetric lower range;
                    // pure-lower → add the symmetric upper. Mixed / partial
                    // ranges fall back to per-letter Byte items to stay
                    // strictly correct.
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
