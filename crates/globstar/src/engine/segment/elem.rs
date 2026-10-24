//! The graph's elements: what each node of a pattern's graph takes of
//! a path segment (§1 of `references/theory/02-segment-engine.md`).
//! JS twin: `packages/globstar/src/engine/segment/elem.js`.

use super::Bytes;
use super::wild::{Lens, Wild};

/// The `min_len` of a segment no path segment can fill: one with an
/// escaped `\` on Windows, where a segment never holds a separator, or
/// an in-segment NFA that accepts nothing.
pub(super) const NEVER: u32 = u32::MAX;

/// The entry of an `Elem::Piece` no segment starts in.
pub(super) const INNER: u32 = u32::MAX;

#[derive(Debug, Clone)]
pub(super) enum Elem {
    /// Literal segment (possibly empty; never contains a separator).
    Lit(Bytes),
    /// In-segment wildcard matcher.
    Wild(Wild),
    /// Globstar absorbing ≥ 0 segments, empty ones included.
    G0,
    /// Globstar absorbing ≥ 1 segment (trailing `/**`: `a/**`
    /// matches `a/` but not `a`).
    G1,
    /// Takes no segment: it stands for its successors, which the tails
    /// of a group share through it instead of an edge from each (see
    /// `graph.rs`).
    Join,
    /// A piece of a segment a brace cuts, or a join among pieces: the
    /// glue NFA matches it with the pieces glued to it. Holds its entry
    /// state there when a segment may start in it, else `INNER`.
    Piece(u32),
}

impl Elem {
    #[inline]
    pub(super) fn is_globstar(&self) -> bool {
        matches!(self, Elem::G0 | Elem::G1)
    }

    #[inline]
    pub(super) fn as_lit(&self) -> &[u8] {
        match self {
            Elem::Lit(lit) => lit,
            _ => unreachable!("not a literal"),
        }
    }

    /// Absorbs zero segments too.
    #[inline]
    pub(super) fn nullable(&self) -> bool {
        matches!(self, Elem::G0 | Elem::Join)
    }

    /// Takes every non-empty segment (`match_dir`'s all-below).
    #[inline]
    pub(super) fn any_seg(&self) -> bool {
        match self {
            Elem::Lit(_) | Elem::Join | Elem::Piece(_) => false,
            Elem::Wild(w) => w.any_seg,
            Elem::G0 | Elem::G1 => true,
        }
    }

    /// What it takes through its wildcards alone (dot=true), by
    /// segment length (`match_dir`'s all-below).
    #[inline]
    pub(super) fn lens(&self) -> Lens {
        match self {
            Elem::Lit(_) | Elem::Join | Elem::Piece(_) => Lens::default(),
            Elem::Wild(w) => w.lens(),
            Elem::G0 | Elem::G1 => Lens::ALL,
        }
    }

    /// Can the element consume some segment? (A piece's answer is the
    /// glue NFA's, `GlueNfa::reach`.)
    #[inline]
    pub(super) fn satisfiable(&self) -> bool {
        match self {
            Elem::Wild(w) => w.min_len != NEVER,
            Elem::Join | Elem::Piece(_) => false,
            _ => true,
        }
    }
}
