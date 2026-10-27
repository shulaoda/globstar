#[derive(Debug)]
pub struct Ast {
    pub body: Node,
    pub negated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    Concat(Vec<Node>),
    Literal(Vec<u8>),
    Separator,
    AnyChar,
    Star,
    Globstar,
    Class(CharClass),
    Brace(Vec<Node>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CharClass {
    pub negated: bool,
    pub ranges: Vec<(u8, u8)>,
}

impl Node {
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

    pub(crate) fn seq(&self) -> &[Node] {
        match self {
            Node::Concat(xs) => xs,
            other => std::slice::from_ref(other),
        }
    }

    pub(crate) fn into_seq(self) -> Vec<Node> {
        match self {
            Node::Concat(xs) => xs,
            other => vec![other],
        }
    }

    pub(crate) fn of_seq(mut nodes: Vec<Node>) -> Node {
        match nodes.len() {
            1 => nodes.pop().unwrap(),
            _ => Node::Concat(nodes),
        }
    }

    pub(crate) fn ends_with_globstar(&self) -> bool {
        match self {
            Node::Globstar => true,
            Node::Concat(xs) => ends_in_globstar(xs, false),
            Node::Brace(bs) => bs.iter().any(Node::ends_with_globstar),
            _ => false,
        }
    }

    pub(crate) fn nullable(&self) -> bool {
        match self {
            Node::Brace(xs) => xs.iter().any(Node::nullable),
            Node::Concat(xs) => xs.iter().all(Node::nullable),
            _ => false,
        }
    }
}

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
