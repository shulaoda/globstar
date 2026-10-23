//! The chain index: which chains of a big union a path may match, by
//! what its segments are, end with, start with or hold. Each chain is
//! filed under the literal strings one of its anchored segments must
//! have — its last (a tail), else its first (a head), else a block's
//! first — read off that segment's nodes when the graph is built; the
//! literal paths' set under their first segments. A path looks up its
//! own last, first and inner segments and walks only the chains they
//! pick: the walk decides, the index only chooses which chains to ask.

use crate::bytes::{fold, seg_end, seg_start};
use crate::parse::ast::Node;

use super::Elem;
use super::chain::{Chain, Shape};
use super::graph::Graph;

/// A union of at most this many chains (the literal set aside) tries
/// them in turn after their gates, which costs less than the lookups.
pub(super) const LINEAR_CHAINS: usize = 10;

/// A segment's strings multiply past this many no more: a product of
/// alternatives (`[._]s[a-v][a-z]`) is cut to the nodes that fit; a
/// single list (a merged set of names) is kept up to the key room.
const MAX_KEY_PRODUCT: usize = 64;

/// The keys a union's segments may give, and the entries its index may
/// file: one per unit of the pattern's weight, never fewer than 4096,
/// so the index grows with the pattern, not with its chains times their
/// keys. A segment whose keys pass what is left has none; an index past
/// it is not built.
pub(super) fn key_room(weight: usize) -> usize {
    weight.max(4096)
}

/// A key compares this many outer bytes of a segment at most (the last
/// of a suffix, the first of a prefix): the walk checks the rest.
const KEY_BYTES: usize = 4;

/// An infix is at most this many first bytes of a literal: the search
/// for it costs at most that many compares per byte of a path's last
/// segment, and the walk checks the rest.
const MAX_INFIX: usize = 16;

/// How a segment's strings bind it: every match is one of them, ends
/// with one, or starts with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Exact,
    Suffix,
    Prefix,
}

/// A string by its length and outer bytes, folded when
/// case-insensitive: `back` holds its last `KEY_BYTES` (the last
/// lowest), `front` its first (the first lowest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    len: usize,
    back: u32,
    front: u32,
}

impl Key {
    const EMPTY: Key = Key {
        len: 0,
        back: 0,
        front: 0,
    };

    fn of(bytes: &[u8], ci: bool) -> Key {
        Key {
            len: bytes.len(),
            back: pack(bytes.iter().rev().copied(), ci),
            front: pack(bytes.iter().copied(), ci),
        }
    }

    /// `self` then `next`.
    fn then(self, next: Key) -> Key {
        let back = if next.len >= KEY_BYTES {
            next.back
        } else {
            next.back | self.back << (8 * next.len)
        };
        let front = if self.len >= KEY_BYTES {
            self.front
        } else {
            self.front | next.front << (8 * self.len)
        };
        Key {
            len: self.len + next.len,
            back,
            front,
        }
    }
}

/// What a segment binds its matches to: strings, each by how it binds,
/// or a literal.
#[derive(Debug)]
pub(super) enum SegKeys {
    Strings(Vec<(Kind, Key)>),
    /// Every match holds this literal (`*.so.*` holds `.so.`): its first
    /// `MAX_INFIX` bytes, folded.
    Infix(Box<[u8]>),
}

impl SegKeys {
    /// What it takes of the key room.
    pub(super) fn len(&self) -> usize {
        match self {
            SegKeys::Strings(keys) => keys.len(),
            SegKeys::Infix(_) => 1,
        }
    }
}

/// Buffers `segment_keys` reuses from one segment to the next.
#[derive(Debug, Default)]
pub(super) struct KeyScratch {
    suffixes: Vec<Key>,
    prefixes: Vec<Key>,
    alts: Vec<Key>,
    next: Vec<Key>,
}

/// The keys of a segment's nodes (`one_kind`), none when they are more
/// than `room`. A segment that is one brace its branches bind apart — a
/// union's members of one segment, `{Makefile,inc.*,*.pc}` — is bound
/// by all their keys, each its own kind; a branch without strings
/// leaves it none.
pub(super) fn segment_keys(
    nodes: &[&Node],
    ci: bool,
    room: usize,
    k: &mut KeyScratch,
) -> Option<SegKeys> {
    let keys = one_kind(nodes, ci, room, k);
    let (None, [Node::Brace(branches)]) = (&keys, nodes) else {
        return keys.filter(|keys| keys.len() <= room);
    };
    let mut out = Vec::new();
    for branch in branches {
        let seq: Vec<&Node> = branch.seq().iter().collect();
        match segment_keys(&seq, ci, room - out.len(), k)? {
            SegKeys::Strings(keys) => out.extend(keys),
            SegKeys::Infix(_) => return None,
        }
    }
    Some(SegKeys::Strings(out))
}

/// One kind of keys for a segment's nodes: its strings when it is
/// literal (`.DS_Store`, `[Dd]ebug`, `{dist,out}`); else those of the
/// literal nodes that end it or that start it, whichever bind more
/// bytes (`*.py[cod]` ends with `.pyc`, `.pyo` or `.pyd`; `._*` starts
/// with `._`); else the start of its longest literal. None when it has
/// no literal, or a string may be empty.
fn one_kind(nodes: &[&Node], ci: bool, room: usize, k: &mut KeyScratch) -> Option<SegKeys> {
    let KeyScratch {
        suffixes,
        prefixes,
        alts,
        next,
    } = k;
    let from_end = nodes.iter().rev().copied();
    let taken = literal_run(from_end, true, ci, room, suffixes, alts, next);
    if taken == nodes.len() {
        return strings(Kind::Exact, suffixes);
    }
    literal_run(nodes.iter().copied(), false, ci, room, prefixes, alts, next);
    let shortest = |keys: &[Key]| keys.iter().map(|k| k.len).min().unwrap_or(0);
    match (shortest(suffixes), shortest(prefixes)) {
        (0, 0) => {
            let longest = nodes
                .iter()
                .filter_map(|n| match n {
                    Node::Literal(bytes) => Some(bytes),
                    _ => None,
                })
                .max_by_key(|bytes| bytes.len())?;
            Some(SegKeys::Infix(
                longest
                    .iter()
                    .take(MAX_INFIX)
                    .map(|&b| fold(b, ci))
                    .collect(),
            ))
        }
        (s, p) if p > s => strings(Kind::Prefix, prefixes),
        _ => strings(Kind::Suffix, suffixes),
    }
}

fn strings(kind: Kind, keys: &[Key]) -> Option<SegKeys> {
    (!keys.iter().any(|k| k.len == 0))
        .then(|| SegKeys::Strings(keys.iter().map(|&k| (kind, k)).collect()))
}

/// The strings of the literal nodes `nodes` yields from one end, as
/// many as fit `MAX_KEY_PRODUCT`, into `out`; how many nodes they took.
/// `back`: the nodes come last first, and each one's strings go in
/// front. A node of one string waits in `fixed` for the next choice, so
/// a run after a long list passes over the list once.
fn literal_run<'a>(
    nodes: impl Iterator<Item = &'a Node>,
    back: bool,
    ci: bool,
    room: usize,
    out: &mut Vec<Key>,
    alts: &mut Vec<Key>,
    next: &mut Vec<Key>,
) -> usize {
    // `key`, then the string of the node read after it.
    let join = |key: Key, node: Key| if back { node.then(key) } else { key.then(node) };
    // The strings so far, then the ones waiting.
    let flush = |out: &mut Vec<Key>, fixed: &mut Key| {
        if fixed.len > 0 {
            for o in out.iter_mut() {
                *o = join(*o, *fixed);
            }
            *fixed = Key::EMPTY;
        }
    };
    out.clear();
    out.push(Key::EMPTY);
    let mut fixed = Key::EMPTY;
    let mut taken = 0;
    for node in nodes {
        alts.clear();
        if !alternatives(node, ci, room, alts) {
            break;
        }
        if let [one] = alts[..] {
            fixed = join(fixed, one);
        } else {
            if out.len() > 1 && out.len() * alts.len() > MAX_KEY_PRODUCT {
                break;
            }
            flush(out, &mut fixed);
            next.clear();
            for &o in out.iter() {
                next.extend(alts.iter().map(|&a| join(o, a)));
            }
            std::mem::swap(out, next);
        }
        taken += 1;
    }
    flush(out, &mut fixed);
    taken
}

/// The strings one literal node matches, into `out`: a literal, an
/// ASCII class that is not negated (its bytes), a brace of literal
/// branches. `false` for any other node, and for a brace of more than
/// `room` strings.
fn alternatives(node: &Node, ci: bool, room: usize, out: &mut Vec<Key>) -> bool {
    match node {
        Node::Literal(bytes) => out.push(Key::of(bytes, ci)),
        Node::Class(class) if !class.negated => {
            for &(lo, hi) in &class.ranges {
                if !hi.is_ascii()
                    || out.len() + usize::from(hi.saturating_sub(lo)) >= MAX_KEY_PRODUCT
                {
                    return false;
                }
                out.extend((lo..=hi).map(|b| Key::of(&[b], ci)));
            }
        }
        Node::Brace(branches) => {
            for branch in branches {
                match branch.seq() {
                    [Node::Literal(bytes)] => out.push(Key::of(bytes, ci)),
                    // A branch of several nodes: rare, so it gets its own
                    // buffers.
                    seq => {
                        let (mut keys, mut alts, mut next) = (Vec::new(), Vec::new(), Vec::new());
                        let taken = literal_run(
                            seq.iter(),
                            false,
                            ci,
                            room,
                            &mut keys,
                            &mut alts,
                            &mut next,
                        );
                        if taken < seq.len() {
                            return false;
                        }
                        out.extend_from_slice(&keys);
                    }
                }
                if out.len() > room {
                    return false;
                }
            }
        }
        _ => return false,
    }
    true
}

/// Which segment of a path a chain's key is looked up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// The last segment: the chain's last element takes it.
    Last = 0,
    /// The first: the chain's first element takes it.
    First = 1,
    /// Any but the last: a block's first element takes one of them.
    Inner = 2,
}

/// The lookups of one role: an exact key of the segment's length (0),
/// a suffix of each length up to `KEY_BYTES` (1 to 4), a prefix of
/// each (5 to 8).
const LOOKUPS: usize = 1 + 2 * KEY_BYTES;

/// The chains of a union filed by their keys.
#[derive(Debug, Clone)]
pub(super) struct ChainIndex {
    /// Open addressing by `hash`, at most one entry in two slots: per
    /// entry a tag of its key's hash and the chain (6 bits), `0` where
    /// there is none. A key is the lookup, an exact key's length and the
    /// outer bytes compared; two keys sharing a tag pick a chain more,
    /// which its walk turns down.
    slots: Box<[u32]>,
    shift: u32,
    /// Per role, the lookups that find something (a bit each), and per
    /// lookup a sketch of the outer bytes filed (`b % 64`): a segment
    /// whose outer byte misses it skips the lookup.
    lookups: [u16; 3],
    sketch: [[u64; LOOKUPS]; 3],
    /// The chains a literal anywhere in their last segment picks.
    infix: Box<[(u32, Box<[u8]>)]>,
    /// The chains no key files: tried on every call, last.
    rest: u64,
}

impl ChainIndex {
    /// The index of a union's chains over `graph`, `None` when they are
    /// few or their entries pass the key room. The graph's keys are the
    /// wildcard elements'; a literal element's key is itself.
    pub(super) fn new(chains: &[Chain], graph: &Graph, ci: bool) -> Option<Box<Self>> {
        let (elems, keys) = (&graph.elems, &graph.keys);
        let set = match &chains.first()?.shape {
            Shape::Set(set) => Some(set),
            _ => None,
        };
        let from = usize::from(set.is_some());
        // A chain is a bit of a word: 64 wildcard chains and the set are
        // one too many, and stay a scan.
        if chains.len() - from <= LINEAR_CHAINS || chains.len() > 64 {
            return None;
        }
        let mut index = ChainIndex {
            slots: Box::default(),
            shift: 0,
            lookups: [0; 3],
            sketch: [[0; LOOKUPS]; 3],
            infix: Box::default(),
            rest: 0,
        };
        let mut entries = Vec::new();
        let mut infix = Vec::new();
        // The literal paths, by their first segments.
        if let Some(set) = set {
            for path in set.iter() {
                let first = path.split(|&b| b == b'/').next().unwrap_or_default();
                index.file(
                    Role::First,
                    Kind::Exact,
                    Key::of(first, ci),
                    0,
                    &mut entries,
                );
            }
        }
        for (c, chain) in chains.iter().enumerate().skip(from) {
            let mut filed = false;
            for (role, node) in chain.anchors() {
                match &elems[node as usize] {
                    Elem::Lit(lit) if !lit.is_empty() => {
                        index.file(role, Kind::Exact, Key::of(lit, ci), c, &mut entries);
                        filed = true;
                    }
                    Elem::Wild(_) => {
                        if let Ok(i) = keys.binary_search_by_key(&node, |&(n, _)| n) {
                            match &keys[i].1 {
                                SegKeys::Strings(strings) => {
                                    for &(kind, key) in strings {
                                        index.file(role, kind, key, c, &mut entries);
                                    }
                                    filed = true;
                                }
                                // Only a last segment is searched for a literal.
                                SegKeys::Infix(lit) if role == Role::Last => {
                                    infix.push((c as u32, lit.clone()));
                                    filed = true;
                                }
                                SegKeys::Infix(_) => {}
                            }
                        }
                    }
                    _ => {}
                }
                if filed {
                    break;
                }
            }
            if !filed {
                index.rest |= 1 << c;
            }
            if entries.len() > key_room(graph.weight) {
                return None;
            }
        }
        entries.sort_unstable();
        entries.dedup();
        let bits = (2 * entries.len())
            .next_power_of_two()
            .trailing_zeros()
            .max(1);
        index.shift = 64 - bits;
        let mut slots = vec![0u32; 1 << bits];
        let last = slots.len() - 1;
        for (key, c) in entries {
            let (mut at, tag) = index.hash(key);
            while slots[at] != 0 {
                at = (at + 1) & last;
            }
            slots[at] = tag | c;
        }
        index.slots = slots.into_boxed_slice();
        index.infix = infix.into_boxed_slice();
        Some(Box::new(index))
    }

    /// File chain `c` under `key` for `role`.
    fn file(&mut self, role: Role, kind: Kind, key: Key, c: usize, entries: &mut Vec<(u64, u32)>) {
        let short = key.len.min(KEY_BYTES);
        let (lookup, len, outer, compared) = match kind {
            Kind::Exact => (0, key.len, key.back, key.len),
            Kind::Suffix => (short, 0, key.back, short),
            Kind::Prefix => (KEY_BYTES + short, 0, key.front, short),
        };
        let r = role as usize;
        self.lookups[r] |= 1 << lookup;
        self.sketch[r][lookup] |= 1 << (outer & 63);
        entries.push((lookup_key(lookup, len, outer & mask(compared)), c as u32));
    }

    /// A key's first slot, and its tag: 26 bits of its hash the slot
    /// does not use, the top one set, above the chain's 6.
    fn hash(&self, key: u64) -> (usize, u32) {
        let h = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        ((h >> self.shift) as usize, ((h >> 8) as u32 | 1 << 25) << 6)
    }

    /// Does a chain the path picks match it (`matches` decides for each)?
    /// The chains its last segment picks are tried first, then those of
    /// its first, of its inner ones, then those without a key; each
    /// chain once, in chain order within a step.
    pub(super) fn any(
        &self,
        path: &[u8],
        ci: bool,
        mut matches: impl FnMut(usize) -> bool,
    ) -> bool {
        // The chains tried so far: a chain two steps pick runs once.
        let mut tried = 0u64;
        macro_rules! try_chains {
            ($chains:expr) => {
                let mut todo = $chains;
                while todo != 0 {
                    let c = todo.trailing_zeros() as usize;
                    todo &= todo - 1;
                    if matches(c) {
                        return true;
                    }
                }
            };
        }
        macro_rules! try_picked {
            ($picked:expr) => {
                let todo = $picked & !tried;
                tried |= todo;
                try_chains!(todo);
            };
        }
        let last_start = seg_start(path, path.len());
        let last = &path[last_start..];
        let mut picked = self.probe(Role::Last, last, ci);
        for (c, lit) in &self.infix {
            if last
                .windows(lit.len())
                .any(|w| w.iter().zip(lit).all(|(&a, &b)| fold(a, ci) == b))
            {
                picked |= 1 << c;
            }
        }
        try_picked!(picked);
        if self.lookups[Role::First as usize] != 0 {
            try_picked!(self.probe(Role::First, &path[..seg_end(path, 0)], ci));
        }
        if self.lookups[Role::Inner as usize] != 0 {
            let mut start = 0;
            while start < last_start {
                let end = seg_end(path, start);
                try_picked!(self.probe(Role::Inner, &path[start..end], ci));
                start = end + 1;
            }
        }
        // No key picks these, so no step tried them.
        try_chains!(self.rest);
        false
    }

    /// The chains `seg` picks for keys of `role`, a bit each.
    #[inline]
    fn probe(&self, role: Role, seg: &[u8], ci: bool) -> u64 {
        let r = role as usize;
        let n = seg.len();
        let mut todo = self.lookups[r];
        // A segment's start is packed only for a prefix lookup.
        let back = pack(seg.iter().rev().copied(), ci);
        let front = if todo >> (KEY_BYTES + 1) != 0 {
            pack(seg.iter().copied(), ci)
        } else {
            0
        };
        let mut picked = 0;
        while todo != 0 {
            let lookup = todo.trailing_zeros() as usize;
            todo &= todo - 1;
            // The length the key holds, the bytes it compares, its end.
            let (len, compared, outer) = if lookup == 0 {
                (n, n, back)
            } else if lookup <= KEY_BYTES {
                (0, lookup, back)
            } else {
                (0, lookup - KEY_BYTES, front)
            };
            if compared > n || self.sketch[r][lookup] & 1 << (outer & 63) == 0 {
                continue;
            }
            let (mut at, tag) = self.hash(lookup_key(lookup, len, outer & mask(compared)));
            let last = self.slots.len() - 1;
            loop {
                let slot = self.slots[at];
                if slot == 0 {
                    break;
                }
                if slot & !63 == tag {
                    picked |= 1 << (slot & 63);
                }
                at = (at + 1) & last;
            }
        }
        picked
    }
}

/// A lookup, the length an exact key holds and the outer bytes compared,
/// in one word. A length past 255 runs into the lookup's bits, where it
/// can meet only a suffix or prefix key: a chain more, which its walk
/// turns down.
fn lookup_key(lookup: usize, len: usize, outer: u32) -> u64 {
    (lookup as u64) << 40 | (len as u64) << 32 | u64::from(outer)
}

/// The first `KEY_BYTES` of `bytes`, folded, the first lowest.
fn pack(bytes: impl Iterator<Item = u8>, ci: bool) -> u32 {
    let mut w = 0;
    for (i, b) in bytes.take(KEY_BYTES).enumerate() {
        w |= u32::from(fold(b, ci)) << (8 * i);
    }
    w
}

/// The low `len` bytes of a word.
fn mask(len: usize) -> u32 {
    if len >= KEY_BYTES {
        u32::MAX
    } else {
        (1 << (8 * len)) - 1
    }
}
