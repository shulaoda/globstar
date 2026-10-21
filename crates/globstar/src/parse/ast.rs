#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ast {
    /// An odd number of leading `!`.
    pub negated: bool,
    pub body: Node,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Concatenation of zero or more nodes. The parser never produces one
    /// of a single node, nor one inside another; the empty one is an empty
    /// brace branch.
    Concat(Vec<Node>),
    /// Literal bytes — no metacharacters; consecutive literals are merged.
    Literal(Vec<u8>),
    /// Path separator `/`.
    Separator,
    /// `?` — one non-separator byte.
    AnyChar,
    /// `*` — zero or more non-separator bytes.
    Star,
    /// `**` — zero or more bytes across separators. The parser emits it
    /// only where it owns a whole segment (§9.1), in every brace expansion;
    /// a `**` that does not is a [`Node::Star`].
    Globstar,
    /// `[...]` character class.
    Class(CharClass),
    /// `{a,b,c}` brace alternation: always two or more branches (§10.4).
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

/// The oracle the byte-set tests check against.
#[cfg(test)]
impl ClassItem {
    pub fn matches(self, b: u8) -> bool {
        match self {
            Self::Byte(x) => x == b,
            Self::Range(lo, hi) => lo <= b && b <= hi,
        }
    }
}

#[cfg(test)]
impl CharClass {
    pub fn matches(&self, b: u8) -> bool {
        if crate::bytes::is_sep(b) {
            return false;
        }
        let listed = self.items.iter().any(|it| it.matches(b));
        listed ^ self.negated
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

    /// Literals and separators only. A sequence holds no sequence, so
    /// this and [`Node::to_literal_bytes`] read one level.
    pub fn is_pure_literal(&self) -> bool {
        self.seq()
            .iter()
            .all(|n| matches!(n, Node::Literal(_) | Node::Separator))
    }

    /// The bytes of a pure literal, `/` for a separator, sized once.
    pub fn to_literal_bytes(&self) -> Option<Vec<u8>> {
        let mut len = 0;
        for node in self.seq() {
            match node {
                Node::Literal(bytes) => len += bytes.len(),
                Node::Separator => len += 1,
                _ => return None,
            }
        }
        let mut out = Vec::with_capacity(len);
        for node in self.seq() {
            match node {
                Node::Literal(bytes) => out.extend_from_slice(bytes),
                _ => out.push(b'/'),
            }
        }
        Some(out)
    }

    /// The node as a sequence: a `Concat`'s children, or the node alone.
    pub(crate) fn seq(&self) -> &[Node] {
        match self {
            Node::Concat(xs) => xs,
            other => std::slice::from_ref(other),
        }
    }

    /// A `*`, `?` or `**`.
    pub(crate) fn is_wildcard(&self) -> bool {
        matches!(self, Node::Star | Node::AnyChar | Node::Globstar)
    }

    /// [`Node::seq`], owned.
    pub(crate) fn into_seq(self) -> Vec<Node> {
        match self {
            Node::Concat(xs) => xs,
            other => vec![other],
        }
    }

    /// A sequence as one node: its only node, or a `Concat`.
    pub(crate) fn of_seq(mut nodes: Vec<Node>) -> Node {
        match nodes.len() {
            1 => nodes.pop().unwrap(),
            _ => Node::Concat(nodes),
        }
    }

    /// Does the node end in a `**` (in every expansion of a brace, in
    /// some)? Such a `**` reads the `/` after the node as its `**/`.
    pub(crate) fn ends_with_globstar(&self) -> bool {
        match self {
            Node::Globstar => true,
            Node::Concat(xs) => xs.last().is_some_and(Node::ends_with_globstar),
            Node::Brace(bs) => bs.iter().any(Node::ends_with_globstar),
            _ => false,
        }
    }
}
