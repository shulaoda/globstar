//! The chain index: which chains of a big union a path may match, by the
//! literal strings one of its anchored segments must have. A path looks
//! up its last, first and inner segments and walks only the chains they
//! pick.

use crate::bytes::{fold, seg_end, seg_start};
use crate::parse::ast::Node;

use super::chain::{Chain, MAX_CHAINS, Shape};
use super::graph::{Elem, Graph};

/// A union of at most this many chains tries them in turn after their
/// gates, which costs less than the lookups.
pub(super) const LINEAR_CHAINS: usize = 10;

/// A product of alternatives (`[._]s[a-v][a-z]`) is cut to the nodes
/// whose strings fit this many; a single list is kept up to the key room.
const MAX_KEY_PRODUCT: usize = 64;

/// A key compares this many outer bytes of a segment at most; the walk
/// checks the rest. Past 64 chains an exact key compares the whole
/// segment by its hash.
const KEY_BYTES: usize = 4;

/// The base of `Key::whole`.
const BASE: u32 = 0x0100_0193;

/// An infix is at most this many first bytes of a literal.
const MAX_INFIX: usize = 16;

/// The lookups of one role: an exact key (0), a suffix of each length up
/// to `KEY_BYTES` (1 to 4), a prefix of each (5 to 8).
const LOOKUPS: usize = 1 + 2 * KEY_BYTES;

/// The keys a union's segments may give and the entries its index may
/// file: one per unit of the pattern's weight, at least 4,096.
pub(super) fn key_room(weight: usize) -> usize {
    weight.max(4096)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Exact,
    Suffix,
    Prefix,
}

/// A string by its length and bytes, folded when case-insensitive: `back`
/// holds its last `KEY_BYTES` (the last lowest), `front` its first (the
/// first lowest), `whole` a hash of all, `pow` `BASE` to its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    len: usize,
    back: u32,
    front: u32,
    whole: u32,
    pow: u32,
}

#[derive(Debug)]
pub(super) enum SegKeys {
    Strings(Vec<(Kind, Key)>),
    /// Every match holds this literal (`*.so.*` holds `.so.`).
    Infix(Box<[u8]>),
}

#[derive(Debug, Default)]
pub(super) struct KeyScratch {
    suffixes: Vec<Key>,
    prefixes: Vec<Key>,
    alts: Vec<Key>,
    next: Vec<Key>,
}

/// Which segment of a path a chain's key is looked up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// The last: the chain's last element takes it.
    Last = 0,
    /// The first: the chain's first element takes it.
    First = 1,
    /// Any but the last: a block's first element takes one of them.
    Inner = 2,
}

#[derive(Debug, Clone)]
pub(super) struct ChainIndex {
    /// Open addressing by `hash`, at most one entry in two slots: a tag of
    /// the key's hash above the chain id (`id_bits`), 0 where there is none.
    slots: Box<[u32]>,
    shift: u32,
    /// 6 for a union of at most 64 chains, whose picks are bits of a word.
    id_bits: u32,
    /// Per role, the lookups that find something, and per lookup a sketch
    /// of the outer bytes filed.
    lookups: [u16; 3],
    sketch: [[u64; LOOKUPS]; 3],
    /// The chains a literal anywhere in their last segment picks.
    infix: Box<[(u32, Box<[u8]>)]>,
    /// The chains no key files: tried on every call, last.
    rest: Box<[u32]>,
}

impl Key {
    const EMPTY: Key = Key {
        len: 0,
        back: 0,
        front: 0,
        whole: 0,
        pow: 1,
    };

    fn of(bytes: &[u8], ci: bool) -> Key {
        Key {
            len: bytes.len(),
            back: pack(bytes.iter().rev().copied(), ci),
            front: pack(bytes.iter().copied(), ci),
            whole: whole_of(bytes, ci),
            pow: BASE.wrapping_pow(bytes.len() as u32),
        }
    }

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
            whole: self.whole.wrapping_mul(next.pow).wrapping_add(next.whole),
            pow: self.pow.wrapping_mul(next.pow),
        }
    }
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

/// The keys of a segment's nodes, none when they pass `room`. A segment
/// that is one brace bound apart by its branches (`{Makefile,inc.*,*.pc}`)
/// is bound by all their keys, each its own kind.
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

/// One kind of keys: the segment's strings when it is literal; else those
/// of the literal nodes that end it or start it, whichever bind more
/// bytes; else the start of its longest literal. None when it has no
/// literal, or a string may be empty.
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

/// The strings of the literal nodes `nodes` yields from one end, as many
/// as fit `MAX_KEY_PRODUCT`, into `out`; how many nodes they took. A
/// node of one string waits in `fixed` for the next choice.
fn literal_run<'a>(
    nodes: impl Iterator<Item = &'a Node>,
    back: bool,
    ci: bool,
    room: usize,
    out: &mut Vec<Key>,
    alts: &mut Vec<Key>,
    next: &mut Vec<Key>,
) -> usize {
    let join = |key: Key, node: Key| if back { node.then(key) } else { key.then(node) };
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

/// The strings one literal node matches: a literal, a positive ASCII
/// class, a brace of literal branches. `false` for any other node, and
/// for a brace of more than `room` strings.
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

impl ChainIndex {
    /// `None` when the chains are few, their entries pass the key room, or
    /// past 64 chains a call could walk more than `MAX_CHAINS` of them.
    pub(super) fn new(chains: &[Chain], graph: &Graph, ci: bool) -> Option<Box<Self>> {
        let (elems, keys) = (&graph.elems, &graph.keys);
        let set = match &chains.first()?.shape {
            Shape::Set(set) => Some(set),
            _ => None,
        };
        let from = usize::from(set.is_some());
        if chains.len() - from <= LINEAR_CHAINS {
            return None;
        }
        let mut index = ChainIndex {
            slots: Box::default(),
            shift: 0,
            id_bits: (usize::BITS - (chains.len() - 1).leading_zeros()).max(6),
            lookups: [0; 3],
            sketch: [[0; LOOKUPS]; 3],
            infix: Box::default(),
            rest: Box::default(),
        };
        let mut entries = Vec::new();
        let mut infix = Vec::new();
        let mut rest = Vec::new();
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
                rest.push(c as u32);
            }
            if entries.len() > key_room(graph.weight) {
                return None;
            }
        }
        entries.sort_unstable();
        entries.dedup();
        if index.id_bits > 6
            && (rest.len() + infix.len() > MAX_CHAINS
                || entries
                    .chunk_by(|a, b| a.0 == b.0)
                    .any(|g| g.len() > MAX_CHAINS))
        {
            return None;
        }
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
        index.rest = rest.into_boxed_slice();
        Some(Box::new(index))
    }

    fn file(&mut self, role: Role, kind: Kind, key: Key, c: usize, entries: &mut Vec<(u64, u32)>) {
        let short = key.len.min(KEY_BYTES);
        let (lookup, len, outer, compared) = match kind {
            Kind::Exact => (
                0,
                key.len,
                key.back,
                self.exact(key.back, key.len, || key.whole),
            ),
            Kind::Suffix => (short, 0, key.back, key.back & mask(short)),
            Kind::Prefix => (KEY_BYTES + short, 0, key.front, key.front & mask(short)),
        };
        let r = role as usize;
        self.lookups[r] |= 1 << lookup;
        self.sketch[r][lookup] |= 1 << (outer & 63);
        entries.push((lookup_key(role, lookup, len, compared), c as u32));
    }

    /// What an exact key compares: its outer bytes, or past 64 chains the
    /// whole string, so that `n17.js` and `n27.js` file apart.
    #[inline]
    fn exact(&self, back: u32, len: usize, whole: impl FnOnce() -> u32) -> u32 {
        if self.id_bits > 6 {
            whole()
        } else {
            back & mask(len)
        }
    }

    /// A key's first slot, and its tag: the hash bits the slot does not
    /// use, the top one set.
    fn hash(&self, key: u64) -> (usize, u32) {
        let h = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let tag = ((h >> 8) as u32 | 1 << (31 - self.id_bits)) << self.id_bits;
        ((h >> self.shift) as usize, tag)
    }

    /// Does a chain the path picks match it? The last segment's picks are
    /// tried first, then the first's, the inner ones', then the chains
    /// without a key; each chain once.
    pub(super) fn any(
        &self,
        path: &[u8],
        ci: bool,
        mut matches: impl FnMut(usize) -> bool,
    ) -> bool {
        if self.id_bits > 6 {
            return self.any_of_many(path, ci, matches);
        }
        let mut tried = 0u64;
        macro_rules! try_picked {
            ($picked:expr) => {
                let mut todo = $picked & !tried;
                tried |= todo;
                while todo != 0 {
                    let c = todo.trailing_zeros() as usize;
                    todo &= todo - 1;
                    if matches(c) {
                        return true;
                    }
                }
            };
        }
        let last_start = seg_start(path, path.len());
        let last = &path[last_start..];
        let mut picked = 0u64;
        self.probe(Role::Last, last, ci, |c| picked |= 1 << c);
        for c in self.infix_picks(last, ci) {
            picked |= 1 << c;
        }
        try_picked!(picked);
        if self.lookups[Role::First as usize] != 0 {
            let mut picked = 0u64;
            self.probe(Role::First, &path[..seg_end(path, 0)], ci, |c| {
                picked |= 1 << c
            });
            try_picked!(picked);
        }
        if self.lookups[Role::Inner as usize] != 0 {
            let mut start = 0;
            while start < last_start {
                let end = seg_end(path, start);
                let mut picked = 0u64;
                self.probe(Role::Inner, &path[start..end], ci, |c| picked |= 1 << c);
                try_picked!(picked);
                start = end + 1;
            }
        }
        self.rest.iter().any(|&c| matches(c as usize))
    }

    /// `any` past 64 chains: each step's picks sorted and tried once.
    fn any_of_many(&self, path: &[u8], ci: bool, mut matches: impl FnMut(usize) -> bool) -> bool {
        let mut try_picked = |picked: &mut Vec<u32>| {
            picked.sort_unstable();
            picked.dedup();
            let hit = picked.iter().any(|&c| matches(c as usize));
            picked.clear();
            hit
        };
        let mut picked = Vec::new();
        let last_start = seg_start(path, path.len());
        let last = &path[last_start..];
        self.probe(Role::Last, last, ci, |c| picked.push(c));
        picked.extend(self.infix_picks(last, ci));
        if try_picked(&mut picked) {
            return true;
        }
        if self.lookups[Role::First as usize] != 0 {
            self.probe(Role::First, &path[..seg_end(path, 0)], ci, |c| {
                picked.push(c)
            });
            if try_picked(&mut picked) {
                return true;
            }
        }
        if self.lookups[Role::Inner as usize] != 0 {
            let mut start = 0;
            while start < last_start {
                let end = seg_end(path, start);
                self.probe(Role::Inner, &path[start..end], ci, |c| picked.push(c));
                start = end + 1;
            }
        }
        picked.extend_from_slice(&self.rest);
        try_picked(&mut picked)
    }

    fn infix_picks<'a>(&'a self, last: &'a [u8], ci: bool) -> impl Iterator<Item = u32> + 'a {
        self.infix
            .iter()
            .filter(move |(_, lit)| {
                last.windows(lit.len())
                    .any(|w| w.iter().zip(lit).all(|(&a, &b)| fold(a, ci) == b))
            })
            .map(|&(c, _)| c)
    }

    /// Hands `pick` the chains `seg` picks for keys of `role`.
    #[inline]
    fn probe(&self, role: Role, seg: &[u8], ci: bool, mut pick: impl FnMut(u32)) {
        let r = role as usize;
        let n = seg.len();
        let mut todo = self.lookups[r];
        let back = pack(seg.iter().rev().copied(), ci);
        let front = if todo >> (KEY_BYTES + 1) != 0 {
            pack(seg.iter().copied(), ci)
        } else {
            0
        };
        let ids = (1 << self.id_bits) - 1;
        while todo != 0 {
            let lookup = todo.trailing_zeros() as usize;
            todo &= todo - 1;
            let (compared, outer) = if lookup == 0 {
                (n, back)
            } else if lookup <= KEY_BYTES {
                (lookup, back)
            } else {
                (lookup - KEY_BYTES, front)
            };
            if compared > n || self.sketch[r][lookup] & 1 << (outer & 63) == 0 {
                continue;
            }
            let key = if lookup == 0 {
                lookup_key(role, 0, n, self.exact(back, n, || whole_of(seg, ci)))
            } else {
                lookup_key(role, lookup, 0, outer & mask(compared))
            };
            let (mut at, tag) = self.hash(key);
            let last = self.slots.len() - 1;
            loop {
                let slot = self.slots[at];
                if slot == 0 {
                    break;
                }
                if slot & !ids == tag {
                    pick(slot & ids);
                }
                at = (at + 1) & last;
            }
        }
    }
}

/// A role, a lookup, an exact key's length (past 255 as 255) and what it
/// compares, in one word.
fn lookup_key(role: Role, lookup: usize, len: usize, compared: u32) -> u64 {
    (role as u64) << 48 | (lookup as u64) << 40 | (len.min(255) as u64) << 32 | u64::from(compared)
}

/// The folded bytes as a polynomial in `BASE`, the last byte its constant term.
fn whole_of(bytes: &[u8], ci: bool) -> u32 {
    bytes.iter().fold(0, |h: u32, &b| {
        h.wrapping_mul(BASE).wrapping_add(u32::from(fold(b, ci)))
    })
}

/// The first `KEY_BYTES` of `bytes`, folded, the first lowest.
fn pack(bytes: impl Iterator<Item = u8>, ci: bool) -> u32 {
    let mut w = 0;
    for (i, b) in bytes.take(KEY_BYTES).enumerate() {
        w |= u32::from(fold(b, ci)) << (8 * i);
    }
    w
}

fn mask(len: usize) -> u32 {
    if len >= KEY_BYTES {
        u32::MAX
    } else {
        (1 << (8 * len)) - 1
    }
}
