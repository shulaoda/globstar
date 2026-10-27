//! AST → the segment graph. One walk keeps a group per distinct
//! (boundary, partial segment) reaching a point of the pattern, so a
//! brace adds nodes per branch, never a product; past the copy room the
//! segments a brace cuts become glued pieces (theory 03, §3).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};

use crate::engine::bytes::has_escaped_sep;
use crate::parse::ast::Node;

use super::Bytes;
use super::wild::{Affix, GlueNfa, NEVER, Wild, WildKind, compile_wild, concat_lits};

/// The entry of an `Elem::Piece` no segment starts in.
pub(super) const INNER: u32 = u32::MAX;

/// The successor that ends the pattern.
pub(super) const ACCEPT: u32 = u32::MAX;

/// The predecessor of the first nodes.
const START: u32 = u32::MAX;

static STAR: Node = Node::Star;

/// What one kind of expansion may take for a pattern: four units per unit
/// of its weight, at least 64 KiB. Each kind draws from a room of its own,
/// and past it keeps the pattern's form unexpanded.
pub(super) fn room(weight: usize) -> usize {
    weight.saturating_mul(4).max(1 << 16)
}

/// A copied token is compiled again in every copy: a copy costs this many
/// units of the room per unit of its weight.
const COPY_COST: usize = 4;

/// Groups past this many are found by hash.
const LINEAR_GROUPS: usize = 16;

#[derive(Debug, Clone)]
pub(super) enum Elem {
    Lit(Bytes),
    Wild(Wild),
    /// Zero or more segments, empty ones too.
    G0,
    /// One or more segments: a trailing `/**`.
    G1,
    /// Takes no segment; stands for its successors.
    Join,
    /// Its entry state in the glue NFA, or `INNER` when no segment
    /// starts in it.
    Piece(u32),
}

pub(super) struct Graph {
    pub(super) elems: Vec<Elem>,
    /// `None` for a linear pattern, whose nodes follow one another.
    pub(super) edges: Option<Edges>,
    pub(super) weight: usize,
    pub(super) glue: Option<Box<GlueNfa>>,
}

/// CSR successor lists, the start's at position `n`.
pub(super) struct Edges {
    offsets: Vec<u32>,
    targets: Vec<u32>,
}

pub(super) struct Survey {
    /// Separators and globstars, inside braces too.
    boundaries: usize,
    /// No brace holds a boundary or may leave a `**` beside one.
    linear: bool,
    pub(super) weight: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Start,
    Sep,
    /// Inside a segment a brace cut: what follows glues to the tails.
    Glue,
}

/// Paths under construction that agree on everything still to come.
#[derive(Clone)]
struct Group<'a> {
    tails: Vec<u32>,
    state: Boundary,
    buf: Vec<&'a Node>,
    /// A `**/` read but not emitted: a bare `**` right after it absorbs
    /// it (`x/**/**` ≡ `x/**`). While set, `buf` is empty.
    pending_globstar: bool,
    /// A `**` read at a boundary: a globstar if a `/` or the end comes
    /// next, a star otherwise.
    double_star: bool,
    /// Stands for the empty expansions of the in-segment braces read
    /// since the boundary, which leave a `**` beside it (`{a,}**`): lives
    /// while the `**` may yet be a globstar, and goes on alone once it is.
    shadow: bool,
}

/// A pattern with a crossing brace records node `i`'s predecessors as
/// `preds[pred_at[i]..pred_at[i + 1]]` and reuses a node by its
/// (element, predecessors) hash; a linear pattern records its nodes only.
struct Builder<'a> {
    elems: Vec<Elem>,
    linear: bool,
    preds: Vec<u32>,
    pred_at: Vec<u32>,
    /// Per node, whether it goes on the segment of its predecessors;
    /// kept up to the last glued node.
    glued: Vec<bool>,
    /// Per node, a wildcard piece's nodes, for the glue NFA.
    pieces: Vec<Vec<&'a Node>>,
    ends: Vec<u32>,
    memo: HashMap<u64, u32, BuildHasherDefault<Fnv>>,
    room: usize,
    /// What suffix sets may take past four times their nodes.
    suffix_pool: usize,
    dot: bool,
    ci: bool,
}

/// Groups that agree, merged. Past `LINEAR_GROUPS` they are found by a
/// hash of their key, and by a hash of their whole nodes once more than
/// `LINEAR_GROUPS` groups share one key.
#[derive(Default)]
struct Merged<'a> {
    groups: Vec<Group<'a>>,
    index: HashMap<u64, Vec<u32>, BuildHasherDefault<Fnv>>,
    whole: bool,
}

/// FNV-1a.
#[derive(Default)]
struct Fnv(u64);

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

    /// A bound on testing one segment, per byte of it.
    pub(super) fn weight(&self) -> usize {
        match self {
            Elem::Wild(w) => w.weight(),
            _ => 1,
        }
    }

    #[inline]
    pub(super) fn nullable(&self) -> bool {
        matches!(self, Elem::G0 | Elem::Join)
    }

    #[inline]
    pub(super) fn any_seg(&self) -> bool {
        match self {
            Elem::Lit(_) | Elem::Join | Elem::Piece(_) => false,
            Elem::Wild(w) => w.any_seg,
            Elem::G0 | Elem::G1 => true,
        }
    }

    /// Can it take some segment? A piece's answer is the glue NFA's.
    #[inline]
    pub(super) fn satisfiable(&self) -> bool {
        match self {
            Elem::Wild(w) => w.min_len != NEVER,
            Elem::Join | Elem::Piece(_) => false,
            _ => true,
        }
    }
}

impl Edges {
    pub(super) fn succ(&self, i: usize) -> &[u32] {
        &self.targets[self.offsets[i] as usize..self.offsets[i + 1] as usize]
    }
}

impl Graph {
    /// Regroups `body` in place: read it as written first.
    pub(super) fn build(body: &mut Node, survey: Survey, dot: bool, ci: bool) -> Self {
        let Survey {
            boundaries,
            linear,
            weight,
        } = survey;
        // A union's members are paths of their own, never one segment's
        // choice: the names among them make the literal set, the rest
        // their own chains.
        let union = matches!(body, Node::Brace(members) if members.len() > 1);
        let linear = linear && !union;
        if !linear {
            match &mut *body {
                Node::Brace(members) if union => members.iter_mut().for_each(regroup),
                other => regroup(other),
            }
        }
        let mut builder = Builder::new(boundaries, linear, weight, dot, ci);
        let groups = match &*body {
            Node::Brace(members) if union => builder.choice(members, vec![Group::first()]),
            body => builder.walk(body.seq(), vec![Group::first()]),
        };
        for g in groups {
            builder.end(g);
        }
        builder.finish(weight)
    }
}

pub(super) fn survey(nodes: &[Node]) -> Survey {
    let mut boundaries = 0;
    let mut linear = true;
    let mut weight = 0;
    for (i, node) in nodes.iter().enumerate() {
        match node {
            Node::Brace(branches) => {
                // A brace that may be empty beside a `**` makes it a
                // globstar in some expansions and a star in others.
                let next_to = |at: usize| matches!(nodes.get(at), Some(Node::Globstar));
                if node.nullable() && (next_to(i + 1) || i.checked_sub(1).is_some_and(next_to)) {
                    linear = false;
                }
                let before = boundaries;
                weight += 1;
                for b in branches {
                    let inner = survey(b.seq());
                    boundaries += inner.boundaries;
                    weight += inner.weight.max(1);
                }
                linear &= boundaries == before;
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
    }
}

/// A literal weighs its length, a class one plus its ranges, a brace one
/// plus its branches (an empty one too), anything else one.
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

    fn dedupe_tails(&mut self) {
        if self.tails.len() > 1 {
            self.tails.sort_unstable();
            self.tails.dedup();
        }
    }

    fn rebase(&mut self, id: u32, state: Boundary) {
        self.tails.clear();
        self.tails.push(id);
        self.state = state;
        self.pending_globstar = false;
    }

    /// Inside a segment, where a `**` is a star.
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

    /// Each node by kind, a literal by its bytes too.
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

    fn whole_key(&self) -> u64 {
        let mut h = Fnv(self.key());
        self.buf.hash(&mut h);
        h.finish()
    }
}

impl<'a> Builder<'a> {
    fn new(boundaries: usize, linear: bool, weight: usize, dot: bool, ci: bool) -> Self {
        let edges = if linear { 0 } else { boundaries + 1 };
        Builder {
            elems: Vec::with_capacity(boundaries + 1),
            linear,
            preds: Vec::with_capacity(edges),
            pred_at: Vec::with_capacity(edges),
            glued: Vec::new(),
            pieces: Vec::new(),
            ends: Vec::new(),
            memo: HashMap::with_capacity_and_hasher(edges, Default::default()),
            room: room(weight),
            suffix_pool: room(weight),
            dot,
            ci,
        }
    }

    fn finish(mut self, weight: usize) -> Graph {
        let edges = (!self.linear).then(|| self.successors());
        // A join nothing glues to holds no piece: it only stands for its
        // successors. Successors come later, so one pass back sees each
        // join after them.
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

    /// The predecessor lists inverted, the pattern's ends as edges to
    /// `ACCEPT`.
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

    /// A node for `elem` after `tails`, or the existing one.
    fn emit(&mut self, elem: Elem, tails: &[u32], glued: bool) -> u32 {
        if self.linear {
            self.elems.push(elem);
            return self.elems.len() as u32 - 1;
        }
        let key = Self::key(&elem, tails, glued);
        match self.reuse(key, tails, glued) {
            Some(found) if self.same_elem(found, &elem) => found,
            _ => self.push(elem, key, tails, glued),
        }
    }

    /// A node for the literal segment `lit`, built only when it is new.
    fn emit_lit(&mut self, lit: &[u8], tails: &[u32], glued: bool) -> u32 {
        let key = Some(Self::key_of(0, lit, tails, glued));
        match self.reuse(key, tails, glued) {
            Some(found) if matches!(&self.elems[found as usize], Elem::Lit(have) if **have == *lit) => {
                found
            }
            _ => self.push(Elem::Lit(lit.into()), key, tails, glued),
        }
    }

    /// The memo's node for `key`, when `tails` are its predecessors.
    fn reuse(&self, key: Option<u64>, tails: &[u32], glued: bool) -> Option<u32> {
        let found = *self.memo.get(&key?)?;
        let i = found as usize;
        (self.is_glued(i) == glued && self.preds_of(i) == tails).then_some(found)
    }

    fn push(&mut self, elem: Elem, key: Option<u64>, tails: &[u32], glued: bool) -> u32 {
        let id = self.elems.len() as u32;
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

    /// The reuse key of a literal, globstar or join; a wildcard is never
    /// shared.
    fn key(elem: &Elem, tails: &[u32], glued: bool) -> Option<u64> {
        let (tag, bytes): (u8, &[u8]) = match elem {
            Elem::Lit(lit) => (0, lit),
            Elem::G0 => (1, &[]),
            Elem::G1 => (2, &[]),
            Elem::Join => (3, &[]),
            Elem::Wild(_) | Elem::Piece(_) => return None,
        };
        Some(Self::key_of(tag, bytes, tails, glued))
    }

    fn key_of(tag: u8, bytes: &[u8], tails: &[u32], glued: bool) -> u64 {
        let mut h = Fnv::default();
        h.write(&[tag]);
        h.write(bytes);
        h.write(&[glued as u8]);
        for &t in tails {
            h.write(&t.to_le_bytes());
        }
        h.finish()
    }

    fn is_glued(&self, i: usize) -> bool {
        self.glued.get(i).copied().unwrap_or(false)
    }

    fn same_elem(&self, id: u32, elem: &Elem) -> bool {
        match (&self.elems[id as usize], elem) {
            (Elem::Lit(a), Elem::Lit(b)) => a == b,
            (Elem::G0, Elem::G0) | (Elem::G1, Elem::G1) | (Elem::Join, Elem::Join) => true,
            _ => false,
        }
    }

    fn end_after(&mut self, from: u32) {
        if !self.linear {
            self.ends.push(from);
        }
    }

    /// Several tails pass through one join, before a node follows them or
    /// a brace copies them: every other node has one predecessor.
    fn gather(&mut self, g: &mut Group) {
        if g.tails.len() < 2 {
            return;
        }
        g.dedupe_tails();
        if g.tails.len() < 2 {
            return;
        }
        let id = self.emit(Elem::Join, &g.tails, g.state == Boundary::Glue);
        g.tails.clear();
        g.tails.push(id);
    }

    /// Every branch goes on from the groups: the open segments go on
    /// into each, or are cut, and the branches glue to the pieces.
    fn choice(&mut self, branches: &'a [Node], mut groups: Vec<Group<'a>>) -> Vec<Group<'a>> {
        let held: usize = groups
            .iter()
            .flat_map(|g| g.buf.iter().copied())
            .map(node_weight)
            .sum();
        if !self.afford((branches.len() - 1).saturating_mul(held)) {
            groups = self.cut_all(groups);
        }
        for g in groups.iter_mut() {
            self.gather(g);
        }
        let mut merged = Merged::default();
        for branch in branches {
            for g in self.walk(branch.seq(), groups.to_vec()) {
                merged.add(g);
            }
        }
        groups = merged.groups;
        for g in &mut groups {
            g.dedupe_tails();
        }
        groups
    }

    fn walk(&mut self, nodes: &'a [Node], mut groups: Vec<Group<'a>>) -> Vec<Group<'a>> {
        for node in nodes {
            match node {
                Node::Brace(branches) if crosses(node) => groups = self.choice(branches, groups),
                Node::Separator => self.close_all(&mut groups),
                Node::Globstar => {
                    groups.retain(|g| !(g.shadow && g.double_star));
                    for g in groups.iter_mut() {
                        if g.double_star || g.within() {
                            self.text(g);
                            g.buf.push(&STAR);
                        } else {
                            g.double_star = true;
                        }
                    }
                }
                node => {
                    // A brace that may be empty leaves a `**` beside what
                    // follows (`{a,}**`): a shadow goes on as if it were
                    // empty, and other text ends it.
                    let empty = node.nullable();
                    let copies = groups.iter().filter(|g| !g.shadow).count();
                    if copies < groups.len() && !empty {
                        groups.retain(|g| !g.shadow);
                    }
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

    /// Before in-segment text: a `**` before it is a star, and a pending
    /// `**/` is emitted.
    fn text(&mut self, g: &mut Group<'a>) {
        self.settle(g);
        if g.double_star {
            g.double_star = false;
            g.buf.push(&STAR);
        }
    }

    /// Emit a pending `**/`, always a `G0`; after a lone `G0` it is that
    /// one (`**/**/` ≡ `**/`).
    fn settle(&mut self, g: &mut Group<'_>) {
        if !g.pending_globstar {
            return;
        }
        self.gather(g);
        let id = match g.tails[..] {
            [t] if t != START && matches!(self.elems[t as usize], Elem::G0) => t,
            _ => self.emit(Elem::G0, &g.tails, false),
        };
        g.rebase(id, Boundary::Sep);
    }

    /// End the partial segment in a piece that what follows glues to.
    fn cut(&mut self, g: &mut Group<'a>) {
        if g.buf.is_empty() {
            return;
        }
        let id = self.segment(g, true);
        g.rebase(id, Boundary::Glue);
        g.buf.clear();
    }

    fn cut_all(&mut self, groups: Vec<Group<'a>>) -> Vec<Group<'a>> {
        let mut cut = Merged::default();
        for mut g in groups {
            self.cut(&mut g);
            cut.add(g);
        }
        cut.groups
    }

    fn afford(&mut self, weight: usize) -> bool {
        let units = weight.saturating_mul(COPY_COST);
        let fits = units <= self.room;
        if fits {
            self.room -= units;
        }
        fits
    }

    /// Close every group's segment; they all continue as one group after
    /// the separator, and a `**` group takes the `/` as its `**/` and
    /// goes on apart, one group per boundary before it.
    fn close_all(&mut self, groups: &mut Vec<Group<'a>>) {
        // The first group closed is the one the others hand their tails
        // to; so is the first `**` group after each boundary.
        let mut joined: Option<usize> = None;
        let mut starred = [None; 3];
        for i in 0..groups.len() {
            let g = &mut groups[i];
            if g.shadow && !g.double_star {
                g.tails.clear();
                continue;
            }
            let into = if g.double_star {
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
        if let Some(at) = groups.iter().position(|g| !g.pending_globstar) {
            groups[..=at].rotate_right(1);
        }
        for g in groups.iter_mut() {
            g.dedupe_tails();
        }
    }

    /// A bare `**` at the end owns the rest of the path: `G0` only from
    /// the pattern start, since `x/**` needs a segment after the `/`.
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
            self.gather(&mut g);
            let id = self.emit(elem, &g.tails, false);
            self.end_after(id);
        } else if g.state == Boundary::Glue && g.buf.is_empty() {
            for &t in &g.tails {
                self.end_after(t);
            }
        } else {
            self.settle(&mut g);
            let id = self.segment(&mut g, false);
            self.end_after(id);
        }
    }

    /// The node of the group's segment, or of the piece read since the
    /// last cut; a wildcard piece keeps its nodes.
    fn segment(&mut self, g: &mut Group<'a>, cut: bool) -> u32 {
        self.gather(g);
        let glued = g.state == Boundary::Glue;
        if !self.linear
            && let [node] = g.buf[..]
            && let Node::Literal(lit) = node
            && !has_escaped_sep(lit)
        {
            return self.emit_lit(lit, &g.tails, glued);
        }
        let elem = self.close(&g.buf);
        let wild = matches!(elem, Elem::Wild(_));
        let id = self.emit(elem, &g.tails, glued);
        if wild && (glued || cut) {
            self.pieces.resize(id as usize + 1, Vec::new());
            self.pieces[id as usize] = g.buf.clone();
        }
        id
    }

    fn close(&mut self, buf: &[&Node]) -> Elem {
        // An escaped `\` on Windows is a separator no segment holds
        // (GLOB_SPEC §4.2): a wildcard that wants more bytes than any
        // segment has.
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
            return Elem::Wild(compile_wild(buf, self.dot, self.ci, &mut self.suffix_pool));
        }
        Elem::Lit(concat_lits(buf))
    }
}

impl<'a> Merged<'a> {
    /// Out of line: inlined into the recursive walk it costs unions of
    /// many branches 3–5% of their compile time.
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

    fn index_all(&mut self) {
        self.index.clear();
        for (i, o) in self.groups.iter().enumerate() {
            let key = self.key(o);
            self.index.entry(key).or_default().push(i as u32);
        }
    }
}

/// Each crossing brace's in-segment branches as one choice, in place:
/// `{a,b,/}` walks as `{{a,b},/}`.
fn regroup(node: &mut Node) {
    match node {
        Node::Concat(children) => children.iter_mut().for_each(regroup),
        Node::Brace(branches) if branches.iter().any(crosses) => {
            branches.iter_mut().for_each(regroup);
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

fn crosses(node: &Node) -> bool {
    match node {
        Node::Separator | Node::Globstar => true,
        Node::Brace(xs) | Node::Concat(xs) => xs.iter().any(crosses),
        _ => false,
    }
}

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
