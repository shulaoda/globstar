#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ast {
    /// An odd number of leading `!`.
    pub negated: bool,
    pub body: Node,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// `**` — zero or more bytes across separators where a separator, or
    /// the edge of the pattern, is on both sides (§9.1), a star elsewhere.
    /// The parser emits a [`Node::Star`] for a `**` beside text, and this
    /// for one beside a separator, a brace or an edge, which may be either
    /// in different brace expansions.
    Globstar,
    /// `[...]` character class.
    Class(CharClass),
    /// `{a,b,c}` brace alternation: always two or more branches (§10.4).
    Brace(Vec<Node>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CharClass {
    pub negated: bool,
    /// Inclusive byte ranges; a lone byte `b` is `(b, b)`.
    pub ranges: Vec<(u8, u8)>,
}

/// The oracle the byte-set tests check against.
#[cfg(test)]
impl CharClass {
    pub fn matches(&self, b: u8) -> bool {
        if crate::bytes::is_sep(b) {
            return false;
        }
        let listed = self.ranges.iter().any(|&(lo, hi)| lo <= b && b <= hi);
        listed ^ self.negated
    }
}

impl Node {
    /// The bytes of a pure literal, `/` for a separator, sized once. A
    /// sequence holds no sequence, so this reads one level.
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

    /// Does the node end in a `**` in some expansion, with only what may
    /// read as nothing after it? Such a `**` reads the `/` after the node
    /// as its `**/`.
    pub(crate) fn ends_with_globstar(&self) -> bool {
        match self {
            Node::Globstar => true,
            Node::Concat(xs) => ends_in_globstar(xs, false),
            Node::Brace(bs) => bs.iter().any(Node::ends_with_globstar),
            _ => false,
        }
    }

    /// May the node read as nothing (a brace with an empty expansion)?
    pub(crate) fn nullable(&self) -> bool {
        match self {
            Node::Brace(xs) => xs.iter().any(Node::nullable),
            Node::Concat(xs) => xs.iter().all(Node::nullable),
            _ => false,
        }
    }
}

/// [`Node::ends_with_globstar`] for a sequence: `outer` when every node
/// in it may read as nothing, so that what comes before it decides.
pub(crate) fn ends_in_globstar(nodes: &[Node], outer: bool) -> bool {
    for node in nodes.iter().rev() {
        if node.ends_with_globstar() {
            return true;
        }
        if !node.nullable() {
            return false;
        }
    }
    outer
}
