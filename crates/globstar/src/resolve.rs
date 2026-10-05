//! Which `**` are globstars (GLOB_SPEC §7.0, §8.1).
//!
//! A pattern means the union of its brace expansions, and in each one a
//! `**` is a globstar only with a separator, or the edge of the pattern, on
//! both sides; otherwise it is a star. The parser leaves every `**` as it
//! is written. Here [`decide`] turns each one into a globstar or a star by
//! the tokens beside it. A `**` beside a brace meets a different token in
//! every branch, so such braces are first taken apart by
//! [`distribute`](crate::distribute::distribute); [`may_touch`] tells when
//! that can be the case, so every other pattern takes one pass.

use crate::ast::Node;
use crate::distribute::distribute;
use crate::error::GlobError;

/// `budget` is what distribution may still copy (§7.7).
pub(crate) fn resolve_globstars(mut body: Node, budget: &mut usize) -> Result<Node, GlobError> {
    if body.has_globstar() {
        if may_touch(&body) {
            body = distribute(body, budget)?;
        }
        decide(&mut body, true, true);
    }
    Ok(body)
}

/// Is a brace in `node` beside a `**`, or beside another brace when either
/// holds a `**`? Only then can distribution change anything.
fn may_touch(node: &Node) -> bool {
    let seq = match node {
        Node::Concat(seq) => seq,
        node => std::slice::from_ref(node),
    };
    (0..seq.len()).any(|i| match &seq[i..] {
        [Node::Globstar, Node::Brace(_), ..] | [Node::Brace(_), Node::Globstar, ..] => true,
        [a @ Node::Brace(_), b @ Node::Brace(_), ..] if a.has_globstar() || b.has_globstar() => {
            true
        }
        [Node::Brace(branches), ..] => branches.iter().any(may_touch),
        _ => false,
    })
}

/// Decides every `**` in `node`, a pattern or a brace branch: a globstar
/// with a separator (or the edge of the pattern) on both sides, a star
/// otherwise. `before`/`after` tell whether the neighbors of `node` are
/// such boundaries.
fn decide(node: &mut Node, before: bool, after: bool) {
    match node {
        Node::Globstar if !(before && after) => *node = Node::Star,
        Node::Brace(branches) => {
            for branch in branches {
                decide(branch, before, after);
            }
        }
        Node::Concat(seq) => {
            // `**/**` is one `**` (§8.6): a `/`, `**` after a `**` is dropped.
            let mut kept = 0;
            for i in 0..seq.len() {
                if matches!(seq[i], Node::Globstar | Node::Brace(_)) {
                    let left = seq[..kept]
                        .last()
                        .map_or(before, |n| matches!(n, Node::Separator));
                    let right = seq
                        .get(i + 1)
                        .map_or(after, |n| matches!(n, Node::Separator));
                    decide(&mut seq[i], left, right);
                    if matches!(seq[i], Node::Globstar)
                        && matches!(seq[..kept], [.., Node::Globstar, Node::Separator])
                    {
                        kept -= 1;
                        continue;
                    }
                }
                seq.swap(kept, i);
                kept += 1;
            }
            seq.truncate(kept);
            if seq.len() == 1 {
                *node = seq.pop().unwrap();
            }
        }
        _ => {}
    }
}
