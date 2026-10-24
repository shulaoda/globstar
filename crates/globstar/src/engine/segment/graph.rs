//! AST → the segment graph.
//!
//! The walk keeps one *group* per distinct (boundary, partial segment)
//! reaching a point of the pattern, so a brace whose branches end at
//! a `/` costs one node per branch and the continuation is shared;
//! nothing is multiplied out. Branches that start alike share their
//! nodes too (a node is reused when its element and predecessors
//! already exist), which turns a union of names into a trie.
//!
//! A segment a separator-crossing brace's edge falls inside (`x{a/,b}y`
//! reads the segments `xa` and `xby`) is copied into each branch, and
//! what follows the brace onto each segment the branches leave open,
//! while the copies weigh no more than the pattern (`COPY_PER_WEIGHT`).
//! Past that a segment is cut into *pieces* at the edge: `x`, `a`, `b`
//! and `y`, the pieces after `x` *glued* to it and `y` to `b`, and a
//! piece is never copied. So the walk makes every node, edge and token
//! a bounded number of times, whatever the braces: the graph is linear
//! in the pattern, and every pattern the parser accepts compiles. The
//! glued pieces are matched together by one in-segment NFA (`GlueNfa`).
//!
//! A `**` is a globstar where a boundary is on both sides and a star
//! elsewhere, in each brace expansion on its own (GLOB_SPEC §9.1): the
//! walk reads it per group, as the expansions reach it.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};

use crate::bytes::has_escaped_sep;
use crate::parse::ast::Node;

use super::Bytes;
use super::chain_index::{KeyScratch, LINEAR_CHAINS, SegKeys, key_room, segment_keys};
use super::elem::{Elem, NEVER};
use super::seg_nfa::GlueNfa;
use super::wild::{Affix, Wild, WildKind, compile_wild, concat_lits};

/// Successor marker for "the pattern ends here".
pub(super) const ACCEPT: u32 = u32::MAX;

/// Predecessor marker for the graph start.
const START: u32 = u32::MAX;

/// What a `**` read as a star adds to a segment.
static STAR: Node = Node::Star;

/// The graph as the walk leaves it: the nodes and, for a pattern with
/// a separator-crossing brace, their edges. A linear pattern is
/// one path, emitted in order; its edges are implied and never stored.
pub(super) struct Graph {
    pub(super) elems: Vec<Elem>,
    pub(super) edges: Option<Edges>,
    /// The pattern's `node_weight`.
    pub(super) weight: usize,
    /// A crossing brace's wildcard nodes and their keys, in node order:
    /// what the chain index files a union's chains by.
    pub(super) keys: Vec<(u32, SegKeys)>,
    /// The glued pieces' NFA, when a brace cuts a segment.
    pub(super) glue: Option<Box<GlueNfa>>,
}

/// Successor lists in CSR form, the start's last (position `n`).
pub(super) struct Edges {
    offsets: Vec<u32>,
    targets: Vec<u32>,
}

impl Edges {
    pub(super) fn succ(&self, i: usize) -> &[u32] {
        &self.targets[self.offsets[i] as usize..self.offsets[i + 1] as usize]
    }
}

impl Graph {
    /// The graph of `body`, whose `survey` the caller took. The walk
    /// regroups the pattern in place (`regroup`), so what needs it as
    /// written reads it first.
    pub(super) fn build(body: &mut Node, survey: Survey, dot: bool, ci: bool) -> Self {
        let Survey {
            boundaries,
            linear,
            weight,
            paths,
        } = survey;
        // Only a separator-crossing brace can need regrouping.
        if !linear {
            regroup(body);
        }
        let mut b = Builder::new(boundaries, linear, weight, dot, ci);
        // Only a union of more paths than `LINEAR_CHAINS` indexes them.
        if paths > LINEAR_CHAINS {
            b.keys = Some(Box::new(KeyReading {
                keys: Vec::new(),
                room: key_room(weight),
                scratch: KeyScratch::default(),
            }));
        }
        let groups = b.walk(body.seq(), vec![Group::first()]);
        for g in groups {
            b.end(g);
        }
        b.finish(weight)
    }
}

/// What the walk needs to know about the pattern before it starts.
pub(super) struct Survey {
    /// Separators and globstars, inside braces too.
    boundaries: usize,
    /// No brace holds a boundary, nor may leave a `**` beside one: the
    /// pattern is one path.
    linear: bool,
    /// The pattern's `node_weight`.
    pub(super) weight: usize,
    /// At least as many as the paths through the graph: a crossing
    /// brace adds its branches', a sequence multiplies its nodes'.
    paths: usize,
}

/// How many segment boundaries the pattern has (a floor for its
/// elements), whether it has a separator-crossing brace at all, its
/// weight (`node_weight`) and a bound on its paths.
pub(super) fn survey(nodes: &[Node]) -> Survey {
    let mut boundaries = 0;
    let mut linear = true;
    let mut weight = 0;
    let mut paths = 1usize;
    for (i, node) in nodes.iter().enumerate() {
        match node {
            Node::Brace(branches) => {
                // A brace that may be empty beside a `**` makes it a
                // globstar in some expansions and a star in others: two
                // paths (`{a,}**`).
                let next_to = |at: usize| matches!(nodes.get(at), Some(Node::Globstar));
                if node.nullable() && (next_to(i + 1) || i.checked_sub(1).is_some_and(next_to)) {
                    linear = false;
                }
                // A brace crosses iff its branches hold a boundary.
                let before = boundaries;
                let mut branch_paths = 0usize;
                weight += 1;
                for b in branches {
                    let inner = survey(b.seq());
                    boundaries += inner.boundaries;
                    weight += inner.weight.max(1);
                    branch_paths = branch_paths.saturating_add(inner.paths);
                }
                let crosses = boundaries > before;
                linear &= !crosses;
                if crosses {
                    paths = paths.saturating_mul(branch_paths);
                }
            }
            Node::Literal(_) | Node::AnyChar | Node::Star | Node::Class(_) => {
                weight += node_weight(node);
            }
            Node::Separator | Node::Globstar => {
                boundaries += 1;
                weight += 1;
            }
            Node::Concat(_) => unreachable!("a sequence holds no sequence"),
        }
    }
    Survey {
        boundaries,
        linear,
        weight,
        paths,
    }
}

/// What a node weighs: a literal its length, a class one plus its
/// ranges, a brace one plus its branches (an empty one too), anything
/// else one. The size bounds that pick a representation scale with it.
pub(super) fn node_weight(node: &Node) -> usize {
    match node {
        Node::Literal(bytes) => bytes.len(),
        Node::Class(class) => 1 + class.ranges.len(),
        Node::Brace(branches) => {
            1 + branches
                .iter()
                .map(|b| b.seq().iter().map(node_weight).sum::<usize>().max(1))
                .sum::<usize>()
        }
        _ => 1,
    }
}

/// The room for copies: a segment a crossing brace's edge falls inside
/// is copied into its branches, and text after the brace onto each
/// segment they leave open (`src/{a/x,b/y}.ts`), while all the copies
/// weigh at most this much per unit of pattern weight, never less than
/// `MIN_COPY_ROOM`. Past that the segments are cut into pieces, which
/// copy nothing, so the graph stays linear in the pattern. Copies keep a
/// pattern's segments whole, as written, and so its chains and literal
/// sets; pieces are for braces whose segments multiply.
const COPY_PER_WEIGHT: usize = 1;
const MIN_COPY_ROOM: usize = 1 << 14;

/// What the last node left behind for the next element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    /// Pattern start.
    Start,
    /// After a separator or a `**/`.
    Sep,
    /// Inside a segment a crossing brace cut: the tails are its pieces
    /// so far, and what follows glues to them.
    Glue,
}

/// Paths under construction that agree on everything still to come:
/// the nodes awaiting a successor (`START` for the graph start), the
/// boundary just crossed and the in-segment nodes read since.
#[derive(Clone)]
struct Group<'a> {
    tails: Vec<u32>,
    state: Boundary,
    buf: Vec<&'a Node>,
    /// A `**/` read but not yet emitted: a bare `**` right after it
    /// absorbs it (`x/**/**` ≡ `x/**`), anything else makes it real.
    /// More of them add nothing (`**/**/` ≡ `**/`). While set, `state`
    /// is the boundary before it and `buf` is empty.
    pending_globstar: bool,
    /// A `**` read at a boundary, `buf` empty: a globstar if a `/` or
    /// the end of the pattern comes next, a star otherwise (§9.1). The
    /// next token decides, in each branch it may be in.
    double_star: bool,
    /// Stands only for the empty expansions of the in-segment braces read
    /// since the boundary, which leave a `**` next to it (`{a,}**`,
    /// `**{,a}/`): the group that read the braces covers every other
    /// reading, the `**` as a star too, so this one lives only while the
    /// `**` may yet be a globstar, and goes on as a group of its own once
    /// a `/` or the end makes it one.
    shadow: bool,
}

impl Group<'_> {
    fn first() -> Self {
        Self {
            tails: vec![START],
            state: Boundary::Start,
            buf: Vec::new(),
            pending_globstar: false,
            double_star: false,
            shadow: false,
        }
    }

    /// Drop repeated tails, which merges and reused nodes add; their
    /// order means nothing (successors are listed by node).
    fn dedupe_tails(&mut self) {
        if self.tails.len() > 1 {
            self.tails.sort_unstable();
            self.tails.dedup();
        }
    }

    /// Continue as the one group after `id`, reusing the vectors.
    fn rebase(&mut self, id: u32, state: Boundary) {
        self.tails.clear();
        self.tails.push(id);
        self.state = state;
        self.pending_globstar = false;
    }

    /// Inside a segment: a `**` is a star here.
    fn within(&self) -> bool {
        self.state == Boundary::Glue || !self.buf.is_empty()
    }

    fn agrees(&self, other: &Self) -> bool {
        self.state == other.state
            && self.pending_globstar == other.pending_globstar
            && self.double_star == other.double_star
            && self.shadow == other.shadow
            && self.buf.len() == other.buf.len()
            && self
                .buf
                .iter()
                .zip(&other.buf)
                .all(|(a, b)| std::ptr::eq(*a, *b) || a == b)
    }

    /// A hash of what `agrees` compares: each node by kind, a literal
    /// by its bytes too.
    fn key(&self) -> u64 {
        let mut h = Fnv::default();
        h.write(&[
            self.state as u8,
            self.pending_globstar as u8,
            self.double_star as u8,
            self.shadow as u8,
        ]);
        for node in &self.buf {
            std::mem::discriminant(*node).hash(&mut h);
            if let Node::Literal(bytes) = node {
                h.write(bytes);
            }
        }
        h.finish()
    }

    /// The same over all of each node.
    fn whole_key(&self) -> u64 {
        let mut h = Fnv(self.key());
        self.buf.hash(&mut h);
        h.finish()
    }
}

/// The keys read so far, by node, what is left of the key room, and the
/// buffers reading them reuses.
struct KeyReading {
    keys: Vec<(u32, SegKeys)>,
    room: usize,
    scratch: KeyScratch,
}

/// Builds the graph. A pattern with a separator-crossing brace also
/// records every node's predecessors (flat, `START` for the start,
/// node `i` owning `preds[pred_at[i]..pred_at[i + 1]]`), whether it is
/// glued to them, a wildcard piece's nodes, the nodes the pattern may
/// end after, and reuses a node by its (element, predecessors) hash; a
/// linear pattern records its nodes only.
struct Builder<'a> {
    elems: Vec<Elem>,
    /// The wildcard segments' keys and the buffers that read them, for
    /// a union the chain index may file.
    keys: Option<Box<KeyReading>>,
    linear: bool,
    preds: Vec<u32>,
    pred_at: Vec<u32>,
    /// Per node: does it go on the segment of its predecessors? A
    /// group's tails are all glued or none, so this holds for every edge
    /// into the node. Kept up to the last glued node (`is_glued`): empty
    /// while no brace cuts a segment.
    glued: Vec<bool>,
    /// Per node, a wildcard piece's nodes, which `GlueNfa` reads; kept
    /// up to the last such piece, as `glued` is.
    pieces: Vec<Vec<&'a Node>>,
    ends: Vec<u32>,
    memo: HashMap<u64, u32, BuildHasherDefault<Fnv>>,
    /// What is left of the room for copies (`COPY_PER_WEIGHT`).
    room: usize,
    dot: bool,
    ci: bool,
}

impl<'a> Builder<'a> {
    fn new(boundaries: usize, linear: bool, weight: usize, dot: bool, ci: bool) -> Self {
        // A linear pattern stores no edges; a crossing brace has at least
        // one node per boundary, each with a predecessor and a key.
        let edges = if linear { 0 } else { boundaries + 1 };
        Builder {
            elems: Vec::with_capacity(boundaries + 1),
            keys: None,
            linear,
            preds: Vec::with_capacity(edges),
            pred_at: Vec::with_capacity(edges),
            glued: Vec::new(),
            pieces: Vec::new(),
            ends: Vec::new(),
            memo: HashMap::with_capacity_and_hasher(edges, Default::default()),
            room: COPY_PER_WEIGHT.saturating_mul(weight).max(MIN_COPY_ROOM),
            dot,
            ci,
        }
    }

    fn finish(mut self, weight: usize) -> Graph {
        let edges = (!self.linear).then(|| self.successors());
        // A join a cut leaves before branches that each start at a `/`,
        // or nothing after a brace, holds no piece: nothing glues to it,
        // so it only stands for its successors, as any join does. A
        // successor's id is past its predecessors', so one pass back
        // sees each join after what follows it.
        let glue = match &edges {
            Some(e) if !self.glued.is_empty() => {
                let n = self.elems.len();
                self.glued.resize(n, false);
                for i in (0..n).rev() {
                    if matches!(self.elems[i], Elem::Join)
                        && self.glued[i]
                        && !e
                            .succ(i)
                            .iter()
                            .any(|&t| t != ACCEPT && self.glued[t as usize])
                    {
                        self.glued[i] = false;
                    }
                }
                self.pieces.resize(n, Vec::new());
                self.glued.contains(&true).then(|| {
                    Box::new(GlueNfa::new(
                        &mut self.elems,
                        e,
                        &self.glued,
                        &self.pieces,
                        self.ci,
                    ))
                })
            }
            _ => None,
        };
        Graph {
            elems: self.elems,
            edges,
            weight,
            keys: self.keys.map_or_else(Vec::new, |k| k.keys),
            glue,
        }
    }

    fn preds_of(&self, id: usize) -> &[u32] {
        let start = self.pred_at[id] as usize;
        let end = self
            .pred_at
            .get(id + 1)
            .map_or(self.preds.len(), |&e| e as usize);
        &self.preds[start..end]
    }

    /// Invert the predecessor lists into CSR successor lists, the start's
    /// at `n` and the pattern's ends as edges to `ACCEPT`.
    fn successors(&self) -> Edges {
        let n = self.elems.len();
        let slot = |from: u32| if from == START { n } else { from as usize };
        let mut offsets = vec![0u32; n + 2];
        for &from in self.preds.iter().chain(&self.ends) {
            offsets[slot(from) + 1] += 1;
        }
        for i in 0..=n {
            offsets[i + 1] += offsets[i];
        }
        let mut fill = offsets.clone();
        let mut targets = vec![0u32; offsets[n + 1] as usize];
        for id in 0..=n {
            let (froms, to) = if id < n {
                (self.preds_of(id), id as u32)
            } else {
                (&self.ends[..], ACCEPT)
            };
            for &from in froms {
                let at = &mut fill[slot(from)];
                targets[*at as usize] = to;
                *at += 1;
            }
        }
        Edges { offsets, targets }
    }

    /// A node for `elem` after `tails` (`glued` to them or not); an
    /// existing one when a branch already made it.
    fn emit(&mut self, elem: Elem, tails: &[u32], glued: bool) -> u32 {
        let id = self.elems.len() as u32;
        if self.linear {
            self.elems.push(elem);
            return id;
        }
        let key = Self::key(&elem, tails, glued);
        if let Some(&found) = key.and_then(|k| self.memo.get(&k))
            && self.same(found, &elem, tails, glued)
        {
            return found;
        }
        self.elems.push(elem);
        if glued {
            self.glued.resize(id as usize + 1, false);
            self.glued[id as usize] = true;
        }
        self.pred_at.push(self.preds.len() as u32);
        self.preds.extend_from_slice(tails);
        if let Some(k) = key {
            self.memo.entry(k).or_insert(id);
        }
        id
    }

    /// Reuse key of a literal, globstar or join node; wildcards stay
    /// apart.
    fn key(elem: &Elem, tails: &[u32], glued: bool) -> Option<u64> {
        let mut h = Fnv::default();
        match elem {
            Elem::Lit(lit) => {
                h.write(&[0]);
                h.write(lit);
            }
            Elem::G0 => h.write(&[1]),
            Elem::G1 => h.write(&[2]),
            Elem::Join => h.write(&[3]),
            Elem::Wild(_) | Elem::Piece(_) => return None,
        }
        h.write(&[glued as u8]);
        for &t in tails {
            h.write(&t.to_le_bytes());
        }
        Some(h.finish())
    }

    fn is_glued(&self, i: usize) -> bool {
        self.glued.get(i).copied().unwrap_or(false)
    }

    /// Is node `id` this element after these predecessors?
    fn same(&self, id: u32, elem: &Elem, tails: &[u32], glued: bool) -> bool {
        let i = id as usize;
        if self.is_glued(i) != glued || self.preds_of(i) != tails {
            return false;
        }
        match (&self.elems[i], elem) {
            (Elem::Lit(a), Elem::Lit(b)) => a == b,
            (Elem::G0, Elem::G0) | (Elem::G1, Elem::G1) | (Elem::Join, Elem::Join) => true,
            _ => false,
        }
    }

    /// Record that the pattern may end after `from`.
    fn end_after(&mut self, from: u32) {
        if !self.linear {
            self.ends.push(from);
        }
    }

    /// A group about to be copied into each of `ways` branches: its
    /// tails become one join node when that keeps fewer edges, so the
    /// branches share an edge to it instead of one from each tail
    /// (`{a/,}` × k would give k² edges).
    fn join(&mut self, g: &mut Group, ways: usize) {
        if g.tails.len() < 2 {
            return;
        }
        g.dedupe_tails();
        let k = g.tails.len();
        if k * ways <= k + ways {
            return;
        }
        let id = self.emit(Elem::Join, &g.tails, g.state == Boundary::Glue);
        g.tails.clear();
        g.tails.push(id);
    }

    /// Walk a sequence of nodes.
    fn walk(&mut self, nodes: &'a [Node], mut groups: Vec<Group<'a>>) -> Vec<Group<'a>> {
        for node in nodes {
            match node {
                Node::Brace(branches) if crosses(node) => {
                    // A segment the brace's edge falls inside goes on into
                    // every branch (`x{a/,b}`: `xa` and `xb`), or is cut
                    // there and the branches glue to the piece before it.
                    let held: usize = groups
                        .iter()
                        .flat_map(|g| g.buf.iter().copied())
                        .map(node_weight)
                        .sum();
                    if !self.afford((branches.len() - 1).saturating_mul(held)) {
                        groups = self.cut_all(groups);
                    }
                    for g in groups.iter_mut() {
                        self.join(g, branches.len());
                    }
                    let mut out = Merged::default();
                    for branch in branches {
                        for g in self.walk(branch.seq(), groups.to_vec()) {
                            out.add(g);
                        }
                    }
                    groups = out.groups;
                    for g in &mut groups {
                        g.dedupe_tails();
                    }
                }
                Node::Separator => self.close_all(&mut groups),
                Node::Globstar => {
                    groups.retain(|g| !(g.shadow && g.double_star));
                    for g in groups.iter_mut() {
                        if g.double_star || g.within() {
                            // After text, or after a `**`: a star.
                            self.text(g);
                            g.buf.push(&STAR);
                        } else {
                            g.double_star = true;
                        }
                    }
                }
                node => {
                    // A brace that may be empty may leave a boundary, or a
                    // `**`, next to what follows it (`{a,}**`, `**{,a}/`):
                    // a shadow goes on as if it were empty, and other text
                    // ends it.
                    let empty = node.nullable();
                    let copies = groups.iter().filter(|g| !g.shadow).count();
                    if copies < groups.len() && !empty {
                        groups.retain(|g| !g.shadow);
                    }
                    // Text goes on every segment the groups hold, or, past
                    // the room for copies, the segments end in pieces and
                    // the text is one piece glued to them all
                    // (`{a/0,…,a/999}` and a long tail).
                    if copies > 1 && !self.afford((copies - 1).saturating_mul(node_weight(node))) {
                        groups = self.cut_all(groups);
                    }
                    let mut shadows = Vec::new();
                    for g in groups.iter_mut().filter(|g| !g.shadow) {
                        if empty && !g.within() {
                            shadows.push(Group {
                                shadow: true,
                                ..g.clone()
                            });
                        }
                        self.text(g);
                        g.buf.push(node);
                    }
                    groups.append(&mut shadows);
                }
            }
        }
        groups
    }

    /// Ready a group for in-segment text: a `**` before it is a star,
    /// and a pending `**/` is real.
    fn text(&mut self, g: &mut Group<'a>) {
        self.settle(g);
        if g.double_star {
            g.double_star = false;
            g.buf.push(&STAR);
        }
    }

    /// Emit a group's pending `**/`. It is always a `G0`: it absorbs
    /// empty segments too, which makes the separators around it lenient.
    /// After a lone `G0` it is that one (`**/**/` ≡ `**/`).
    fn settle(&mut self, g: &mut Group<'_>) {
        if !g.pending_globstar {
            return;
        }
        let id = match g.tails[..] {
            [t] if t != START && matches!(self.elems[t as usize], Elem::G0) => t,
            _ => self.emit(Elem::G0, &g.tails, false),
        };
        g.rebase(id, Boundary::Sep);
    }

    /// End a group's partial segment in a piece: what comes next glues
    /// to it.
    fn cut(&mut self, g: &mut Group<'a>) {
        if g.buf.is_empty() {
            return;
        }
        let id = self.segment(g, true);
        g.rebase(id, Boundary::Glue);
        g.buf.clear();
    }

    /// Cut every group (`cut`): those that end in a piece agree again.
    fn cut_all(&mut self, groups: Vec<Group<'a>>) -> Vec<Group<'a>> {
        let mut cut = Merged::default();
        for mut g in groups {
            self.cut(&mut g);
            cut.add(g);
        }
        cut.groups
    }

    /// Take `units` of the room for copies, if that much is left.
    fn afford(&mut self, units: usize) -> bool {
        let fits = units <= self.room;
        if fits {
            self.room -= units;
        }
        fits
    }

    /// Close every group's segment into one node per group; they all
    /// continue as one group after the separator. A glued group whose
    /// segment ended with its last piece passes its tails on. A `**`
    /// group takes the `/` as its `**/`, emitted later, and goes on
    /// apart, one group per boundary before it.
    fn close_all(&mut self, groups: &mut Vec<Group<'a>>) {
        // The first group closed is the one they all continue as, and the
        // first `**` group after each boundary the one those continue as:
        // the others hand them their tails. A group left without tails is
        // gone.
        let mut joined: Option<usize> = None;
        let mut starred = [None; 3];
        for i in 0..groups.len() {
            let g = &mut groups[i];
            if g.shadow && !g.double_star {
                g.tails.clear();
                continue;
            }
            let into = if g.double_star {
                // Tails a pending `**/` gathered become one `G0`: kept,
                // they would pile up (`{**,a}/` × k: k² edges).
                if g.tails.len() > 1 {
                    self.settle(g);
                }
                g.double_star = false;
                g.shadow = false;
                g.pending_globstar = true;
                &mut starred[g.state as usize]
            } else {
                if g.state != Boundary::Glue || !g.buf.is_empty() {
                    self.settle(g);
                    let id = self.segment(g, false);
                    g.rebase(id, Boundary::Sep);
                }
                g.buf.clear();
                g.state = Boundary::Sep;
                &mut joined
            };
            match *into {
                Some(j) => {
                    let tails = std::mem::take(&mut groups[i].tails);
                    groups[j].tails.extend_from_slice(&tails);
                }
                None => *into = Some(i),
            }
        }
        groups.retain(|g| !g.tails.is_empty());
        // The joined group first, then the pending `**/`s in order.
        if let Some(at) = groups.iter().position(|g| !g.pending_globstar) {
            groups[..=at].rotate_right(1);
        }
        for g in groups.iter_mut() {
            g.dedupe_tails();
        }
    }

    /// End a group at the end of the pattern. A bare `**` owns the rest
    /// of the path: pending `**/` add nothing to it, and a separator
    /// before them still needs a segment to follow (`x/**/**` ≡
    /// `x/**`), so only the pattern start gives `G0`.
    fn end(&mut self, mut g: Group<'a>) {
        if g.shadow && !g.double_star {
            return;
        }
        if g.double_star {
            let elem = if g.state == Boundary::Start {
                Elem::G0
            } else {
                Elem::G1
            };
            let id = self.emit(elem, &g.tails, false);
            self.end_after(id);
        } else if g.state == Boundary::Glue && g.buf.is_empty() {
            for &t in &g.tails {
                self.end_after(t);
            }
        } else {
            self.settle(&mut g);
            let id = self.segment(&g, false);
            self.end_after(id);
        }
    }

    /// The node of a group's segment, or of the piece of it read since
    /// the last cut, after its tails; a piece when glued to them or `cut`
    /// (what follows glues to it). A crossing brace's wildcard segments
    /// keep their keys (a union's chains are filed by them), and its
    /// wildcard pieces their nodes; a wildcard node is never shared, so
    /// its id is new.
    fn segment(&mut self, g: &Group<'a>, cut: bool) -> u32 {
        let elem = self.close(&g.buf);
        let wild = matches!(elem, Elem::Wild(_));
        let glued = g.state == Boundary::Glue;
        let id = self.emit(elem, &g.tails, glued);
        if wild && (glued || cut) {
            self.pieces.resize(id as usize + 1, Vec::new());
            self.pieces[id as usize] = g.buf.clone();
        }
        if wild
            && let Some(k) = &mut self.keys
            && let Some(keys) = segment_keys(&g.buf, self.ci, k.room, &mut k.scratch)
        {
            k.room -= keys.len();
            k.keys.push((id, keys));
        }
        id
    }

    /// The element of a closed segment.
    fn close(&self, buf: &[&Node]) -> Elem {
        // An escaped `\` on Windows: no segment holds a separator, so a
        // segment with one is a silent miss (§4.2), kept as a wildcard
        // that wants more bytes than any segment has.
        if buf
            .iter()
            .any(|node| matches!(node, Node::Literal(lit) if has_escaped_sep(lit)))
        {
            return Elem::Wild(Wild {
                kind: WildKind::Affix(Affix::new(Bytes::default(), Bytes::default())),
                min_len: NEVER,
                variable: false,
                dot_protect: false,
                any_seg: false,
            });
        }
        if !buf.iter().all(|node| matches!(node, Node::Literal(_))) {
            return Elem::Wild(compile_wild(buf, self.dot, self.ci));
        }
        // Empty, one literal, or literals glued across a brace edge.
        Elem::Lit(concat_lits(buf))
    }
}

/// Groups joined when they agree on boundary, pending `**`, and partial
/// segment: their continuations are the same nodes. A union leaves a
/// group per member, so past `LINEAR_GROUPS` they are found by a hash
/// of that key, and by a hash of whole nodes once more than
/// `LINEAR_GROUPS` groups apart share one (members apart only inside a
/// class or a brace).
#[derive(Default)]
struct Merged<'a> {
    groups: Vec<Group<'a>>,
    index: HashMap<u64, Vec<u32>, BuildHasherDefault<Fnv>>,
    whole: bool,
}

const LINEAR_GROUPS: usize = 16;

impl<'a> Merged<'a> {
    // Kept out of the recursive walk: inlined there, it costs the
    // unions of many branches 3–5% of their compile time.
    #[inline(never)]
    fn add(&mut self, g: Group<'a>) {
        let n = self.groups.len();
        if n == LINEAR_GROUPS {
            self.index_all();
        }
        let mut refile = false;
        let found = if n < LINEAR_GROUPS {
            self.groups.iter().position(|o| o.agrees(&g))
        } else {
            let bucket = self.index.entry(self.key(&g)).or_default();
            let found = bucket
                .iter()
                .map(|&i| i as usize)
                .find(|&i| self.groups[i].agrees(&g));
            if found.is_none() {
                bucket.push(n as u32);
                refile = !self.whole && bucket.len() > LINEAR_GROUPS;
            }
            found
        };
        match found {
            Some(i) => self.groups[i].tails.extend(g.tails),
            None => self.groups.push(g),
        }
        if refile {
            self.whole = true;
            self.index_all();
        }
    }

    fn key(&self, g: &Group<'_>) -> u64 {
        if self.whole { g.whole_key() } else { g.key() }
    }

    /// File every group by its key.
    fn index_all(&mut self) {
        self.index.clear();
        for (i, o) in self.groups.iter().enumerate() {
            let key = self.key(o);
            self.index.entry(key).or_default().push(i as u32);
        }
    }
}

/// Each crossing brace's in-segment branches as one choice, in place:
/// `{a,b,/}` walks as `{{a,b},/}`, the choice one piece, not one per
/// branch.
fn regroup(node: &mut Node) {
    match node {
        Node::Concat(children) => children.iter_mut().for_each(regroup),
        Node::Brace(branches) if branches.iter().any(crosses) => {
            branches.iter_mut().for_each(regroup);
            // The choice takes the first in-segment branch's place.
            if let Some(at) = branches.iter().position(|b| !crosses(b)) {
                let mut within: Vec<Node> = branches.extract_if(at.., |b| !crosses(b)).collect();
                let choice = match within.len() {
                    1 => within.pop().unwrap(),
                    _ => Node::Brace(within),
                };
                branches.insert(at, choice);
            }
        }
        _ => {}
    }
}

/// Does the node hold a segment boundary?
fn crosses(node: &Node) -> bool {
    match node {
        Node::Separator | Node::Globstar => true,
        Node::Brace(xs) | Node::Concat(xs) => xs.iter().any(crosses),
        _ => false,
    }
}

/// FNV-1a over the keys of nodes and groups, most of them a few bytes.
#[derive(Default)]
struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for &b in bytes {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = h;
    }
}
