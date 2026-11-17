//! The pickers: how a union's call finds the chains worth a walk
//! (theory 03, §5). A few chains are tried in turn, each after its gate;
//! more are picked by the chain index.

use crate::engine::bytes::{fold, seg_end, seg_start};

use super::SegmentMatcher;
use super::bits::ByteSketch;
use super::chain::{Chain, Literals, Shape};
use super::graph::{Elem, Graph, room};
use super::graph_nfa::AnyNfa;
use super::wild::{Wild, WildKind};

/// Wildcard chains kept without an index.
pub(super) const MAX_CHAINS: usize = 64;

/// A union of at most this many chains tries them in turn after their
/// gates, which costs less than the lookups.
pub(super) const LINEAR_CHAINS: usize = 10;

const NO_HEAD: u8 = 0;

/// The bytes a match may end with, and the folded first byte a literal
/// head requires (`NO_HEAD`: none).
#[derive(Debug, Clone, Copy)]
pub(super) struct Gate {
    last: ByteSketch,
    head: u8,
}

impl Chain {
    pub(super) fn gate(&self, elems: &[Elem], ci: bool) -> Gate {
        let first = self.nodes.first().map_or(0, |&n| n as usize);
        let last = self.nodes.last().map_or(elems.len() - 1, |&n| n as usize);
        let last = match &elems[last] {
            Elem::Lit(lit) => ByteSketch::of_last_bytes(std::iter::once(&lit[..]), ci),
            Elem::Wild(w) => match &w.kind {
                WildKind::Affix(affix) => {
                    ByteSketch::of_last_bytes(std::iter::once(affix.parts().1), ci)
                }
                WildKind::Suffixes(set) => set.last,
                WildKind::Pieces(_) | WildKind::Generic(_) => ByteSketch::ALL,
            },
            _ => ByteSketch::ALL,
        };
        let head = match &elems[first] {
            Elem::Lit(lit) => lit.first().map_or(NO_HEAD, |&b| fold(b, ci)),
            _ => NO_HEAD,
        };
        Gate { last, head }
    }
}

impl SegmentMatcher {
    /// Out of line: keeps `is_match` small for the other matchers.
    #[inline(never)]
    pub(super) fn any_chain_matches(
        &self,
        gates: &[Gate],
        literals: Option<&Literals>,
        path: &[u8],
    ) -> bool {
        if literals.is_some_and(|set| set.contains(path, self.case_insensitive)) {
            return true;
        }
        let first = path
            .first()
            .map_or(NO_HEAD, |&b| fold(b, self.case_insensitive));
        let mut last_start = None;
        self.chains.iter().zip(gates).any(|(chain, gate)| {
            gate.last.may_end(path)
                && (gate.head == NO_HEAD || gate.head == first)
                && self.ends_like(chain, path, &mut last_start)
                && self.chain_matches(chain, path)
        })
    }

    #[inline]
    fn ends_like(&self, chain: &Chain, path: &[u8], last_start: &mut Option<usize>) -> bool {
        let last = chain
            .nodes
            .last()
            .map_or(self.elems.len() - 1, |&n| n as usize);
        let kind = match &self.elems[last] {
            Elem::Lit(lit) => return self.tail_is(path, lit),
            Elem::Wild(w) => &w.kind,
            _ => return true,
        };
        match kind {
            WildKind::Affix(affix) => match affix.parts() {
                (_, suffix) if !suffix.is_empty() => self.tail_is(path, suffix),
                (prefix, _) => {
                    let start = *last_start.get_or_insert_with(|| seg_start(path, path.len()));
                    path.len() - start >= prefix.len()
                        && self.affix_eq(prefix, &path[start..start + prefix.len()])
                }
            },
            WildKind::Suffixes(_) | WildKind::Pieces(_) | WildKind::Generic(_) => true,
        }
    }

    #[inline]
    fn tail_is(&self, path: &[u8], suffix: &[u8]) -> bool {
        path.len() >= suffix.len() && self.affix_eq(suffix, &path[path.len() - suffix.len()..])
    }
}

// The chain index: which chains of a big union a path may match, by the
// literal strings one of its anchored segments must have. A path looks
// up its last, first and inner segments and walks only the chains they
// pick.

/// A product of alternatives (`[._]s[a-v][a-z]`) is cut to the nodes
/// whose strings fit this many; a single list is kept up to the key room.
pub(super) const MAX_KEY_PRODUCT: usize = 64;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
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
enum SegKeys {
    /// A literal segment's.
    Lit(Key),
    Strings(Vec<(Kind, Key)>),
    /// Every match holds this literal (`*.so.*` holds `.so.`).
    Infix(Box<[u8]>),
}

#[derive(Default)]
struct KeyScratch {
    suffixes: Vec<Key>,
    prefixes: Vec<Key>,
    exact: Vec<Key>,
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
    /// Past 64 the id of all ones is no chain's: it picks the NFA.
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

impl Chain {
    /// The nodes whose segment the shape pins, in the index's lookup
    /// order: the last, the first, a block's first.
    pub(super) fn anchors(&self) -> impl Iterator<Item = (Role, u32)> + '_ {
        let n = self.nodes.len();
        let (head, tail, block) = match self.shape {
            Shape::Line(line) => (line.g as usize, (line.g as usize + 1).min(n), None),
            Shape::Block { g1, g2 } => (g1 as usize, g2 as usize + 1, Some(g1 as usize + 1)),
        };
        let last = (tail < n || head == n && n > 0).then(|| (Role::Last, self.nodes[n - 1]));
        let first = (head > 0).then(|| (Role::First, self.nodes[0]));
        let inner = block.map(|b| (Role::Inner, self.nodes[b]));
        last.into_iter().chain(first).chain(inner)
    }
}

impl SegmentMatcher {
    /// The index names the literal set id 0 and the chains the ids after
    /// it, so a step's picks try the set first.
    #[inline(never)]
    pub(super) fn indexed_chain_matches(
        &self,
        index: &ChainIndex,
        literals: Option<&Literals>,
        nfa: &AnyNfa,
        path: &[u8],
    ) -> bool {
        let ci = self.case_insensitive;
        let matches = |id| match literals {
            None => self.chain_matches(&self.chains[id], path),
            Some(set) if id == 0 => set.contains(path, ci),
            Some(_) => self.chain_matches(&self.chains[id - 1], path),
        };
        index.any(path, ci, matches, || nfa.is_match(self, path))
    }
}

impl Key {
    pub(super) const EMPTY: Key = Key {
        len: 0,
        back: 0,
        front: 0,
        whole: 0,
        pow: 1,
    };

    pub(super) fn of(bytes: &[u8], ci: bool) -> Key {
        Key {
            len: bytes.len(),
            back: pack(bytes.iter().rev().copied(), ci),
            front: pack(bytes.iter().copied(), ci),
            whole: whole_of(bytes, ci),
            pow: BASE.wrapping_pow(bytes.len() as u32),
        }
    }

    pub(super) fn then(self, next: Key) -> Key {
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

/// The keys a wildcard segment's element gives: its strings when nothing
/// varies; else those it must end with or start with, whichever bind more
/// bytes; else the start of its longest literal. None for a brace around
/// wildcards, or when a string may be empty.
fn keys_of(w: &Wild, ci: bool, k: &mut KeyScratch) -> Option<SegKeys> {
    let KeyScratch {
        suffixes,
        prefixes,
        exact,
    } = k;
    suffixes.clear();
    prefixes.clear();
    exact.clear();
    let fixed = !w.variable;
    let mut pieces = None;
    match &w.kind {
        WildKind::Affix(affix) => {
            let (p, s) = affix.parts();
            prefixes.push(Key::of(p, ci));
            suffixes.push(Key::of(s, ci));
        }
        WildKind::Suffixes(set) => {
            let p = Key::of(set.prefix(), ci);
            prefixes.push(p);
            if !set.has_empty() {
                suffixes.extend(set.iter().map(|s| Key::of(s, ci)));
                if fixed && w.min_len as usize == p.len {
                    exact.extend(suffixes.iter().map(|&s| p.then(s)));
                }
            }
        }
        WildKind::Pieces(p) => {
            let whole = p.edge_keys(true, ci, suffixes);
            p.edge_keys(false, ci, prefixes);
            if fixed && whole {
                exact.extend_from_slice(suffixes);
            }
            pieces = Some(p);
        }
        WildKind::Generic(_) => return None,
    }
    if !exact.is_empty() {
        return Some(strings(Kind::Exact, exact));
    }
    let shortest = |keys: &[Key]| keys.iter().map(|k| k.len).min().unwrap_or(0);
    match (shortest(suffixes), shortest(prefixes)) {
        (0, 0) => pieces
            .and_then(|p| p.longest_literal())
            .map(|lit| SegKeys::Infix(lit.into_iter().take(MAX_INFIX).collect())),
        (s, p) if p >= s => Some(strings(Kind::Prefix, prefixes)),
        _ => Some(strings(Kind::Suffix, suffixes)),
    }
}

fn strings(kind: Kind, keys: &[Key]) -> SegKeys {
    SegKeys::Strings(keys.iter().map(|&k| (kind, k)).collect())
}

impl ChainIndex {
    /// `None` when the chains are few, their entries pass the key room, or
    /// past 64 chains more than `MAX_CHAINS` of them file no key.
    pub(super) fn new(
        literals: Option<&Literals>,
        chains: &[Chain],
        graph: &Graph,
        ci: bool,
    ) -> Option<Box<Self>> {
        let elems = &graph.elems;
        let mut scratch = KeyScratch::default();
        if chains.len() <= LINEAR_CHAINS {
            return None;
        }
        // The literal set is filed as id 0, chain `c` as `c + first_chain`.
        let first_chain = usize::from(literals.is_some());
        let ids = chains.len() + first_chain;
        let mut index = ChainIndex {
            slots: Box::default(),
            shift: 0,
            id_bits: if ids > MAX_CHAINS {
                usize::BITS - ids.leading_zeros()
            } else {
                6
            },
            lookups: [0; 3],
            sketch: [[0; LOOKUPS]; 3],
            infix: Box::default(),
            rest: Box::default(),
        };
        let mut entries = Vec::new();
        let mut infix = Vec::new();
        let mut rest = Vec::new();
        if let Some(set) = literals {
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
        let mut read = |node: u32| match &elems[node as usize] {
            Elem::Lit(lit) if !lit.is_empty() => Some(SegKeys::Lit(Key::of(lit, ci))),
            Elem::Wild(w) => keys_of(w, ci, &mut scratch),
            _ => None,
        };
        // Chains that share a node read its keys once, into
        // `keys[slot[node]]`.
        let mut slot = vec![u32::MAX; elems.len()];
        let mut keys: Vec<Option<SegKeys>> = Vec::with_capacity(chains.len());
        for (c, chain) in chains.iter().enumerate() {
            let id = c + first_chain;
            let mut filed = false;
            for (role, node) in chain.anchors() {
                let at = &mut slot[node as usize];
                if *at == u32::MAX {
                    *at = keys.len() as u32;
                    keys.push(read(node));
                }
                match &keys[*at as usize] {
                    Some(SegKeys::Lit(key)) => {
                        index.file(role, Kind::Exact, *key, id, &mut entries);
                        filed = true;
                    }
                    Some(SegKeys::Strings(strings)) => {
                        for &(kind, key) in strings.iter() {
                            index.file(role, kind, key, id, &mut entries);
                        }
                        filed = true;
                    }
                    Some(SegKeys::Infix(lit)) if role == Role::Last => {
                        infix.push((id as u32, lit.clone()));
                        filed = true;
                    }
                    _ => {}
                }
                if filed {
                    break;
                }
            }
            if !filed {
                rest.push(id as u32);
            }
            if entries.len() > room(graph.weight) {
                return None;
            }
        }
        entries.sort_unstable();
        entries.dedup();
        if index.id_bits > 6 {
            if rest.len() + infix.len() > MAX_CHAINS {
                return None;
            }
            // A key of more than `MAX_CHAINS` chains picks the NFA instead.
            let nfa = (1 << index.id_bits) - 1;
            for group in entries.chunk_by_mut(|a, b| a.0 == b.0) {
                if group.len() > MAX_CHAINS {
                    group.fill((group[0].0, nfa));
                }
            }
            entries.dedup();
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
        nfa: impl FnOnce() -> bool,
    ) -> bool {
        if self.id_bits > 6 {
            return self.any_of_many(path, ci, matches, nfa);
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

    /// `any` past 64 chains: each step's picks sorted and tried once, and
    /// the NFA last when a step picked it. The inner segments pick as one
    /// step, and drop their repeats whenever the picks pass twice the ids.
    fn any_of_many(
        &self,
        path: &[u8],
        ci: bool,
        mut matches: impl FnMut(usize) -> bool,
        nfa: impl FnOnce() -> bool,
    ) -> bool {
        let nfa_id = (1 << self.id_bits) - 1;
        let mut nfa_picked = false;
        let mut try_picked = |picked: &mut Vec<u32>| {
            picked.sort_unstable();
            picked.dedup();
            nfa_picked |= picked.last() == Some(&nfa_id);
            let hit = picked.iter().any(|&c| c != nfa_id && matches(c as usize));
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
                if picked.len() > 2 << self.id_bits {
                    drop_repeats(&mut picked, 1 << self.id_bits);
                }
                start = end + 1;
            }
        }
        picked.extend_from_slice(&self.rest);
        try_picked(&mut picked) || nfa_picked && nfa()
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

/// Keeps the first of each id in `picked`.
#[cold]
fn drop_repeats(picked: &mut Vec<u32>, ids: usize) {
    let mut seen = vec![false; ids];
    picked.retain(|&c| !std::mem::replace(&mut seen[c as usize], true));
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
