#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ast {
    pub negation_count: u32,
    pub body: Node,
}

impl Ast {
    pub fn is_negated(&self) -> bool {
        self.negation_count % 2 == 1
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharClass {
    pub negated: bool,
    pub items: Vec<ClassItem>,
}

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

#[inline]
fn ascii_case_alt(b: u8) -> u8 {
    match b {
        b'A'..=b'Z' => b | 0x20,
        b'a'..=b'z' => b & !0x20,
        _ => b,
    }
}

impl CharClass {
    pub fn matches(&self, b: u8) -> bool {
        if std::path::is_separator(b as char) {
            return false;
        }
        let listed = self.items.iter().any(|it| it.matches(b));
        listed ^ self.negated
    }

    pub(crate) fn ci_letter(b: u8) -> Option<Self> {
        let alt = ascii_case_alt(b);
        (alt != b).then(|| CharClass {
            negated: false,
            items: vec![ClassItem::Byte(b), ClassItem::Byte(alt)],
        })
    }

    pub fn expanded_ascii_case_insensitive(&self) -> Self {
        let mut items = Vec::with_capacity(self.items.len() * 2);
        for item in &self.items {
            items.push(*item);
            match *item {
                ClassItem::Byte(b) => {
                    let alt = ascii_case_alt(b);
                    if alt != b {
                        items.push(ClassItem::Byte(alt));
                    }
                }
                ClassItem::Range(lo, hi) => {
                    if lo >= b'A' && hi <= b'Z' {
                        items.push(ClassItem::Range(lo | 0x20, hi | 0x20));
                    } else if lo >= b'a' && hi <= b'z' {
                        items.push(ClassItem::Range(lo & !0x20, hi & !0x20));
                    } else {
                        for b in lo..=hi {
                            let alt = ascii_case_alt(b);
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
    pub fn has_globstar(&self) -> bool {
        match self {
            Node::Globstar => true,
            Node::Concat(xs) | Node::Brace(xs) => xs.iter().any(Node::has_globstar),
            _ => false,
        }
    }

    pub fn is_pure_literal(&self) -> bool {
        match self {
            Node::Literal(_) | Node::Separator => true,
            Node::Concat(xs) => xs.iter().all(Node::is_pure_literal),
            _ => false,
        }
    }

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
