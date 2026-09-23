//! Match finders for LZMA, over a window of up to 4 GB.
//!
//! Like [`crate::lz77`]'s finder, but tuned for LZMA:
//!
//! - Matches can be as short as 2 bytes (worth it at small distances, since
//!   LZMA codes them cheaply), so the most recent position of each 2-byte
//!   and 3-byte string is kept in its own small table.
//! - Every improvement found along the way is reported, not just the best,
//!   so the encoder can prefer a slightly shorter match that is much closer.
//!
//! Longer matches come from the positions whose next 4 bytes hash the same,
//! organised in one of two ways ([`Kind`]):
//!
//! **Hash chains** link each position to the previous one in its bucket.
//! Adding a position is O(1), but a search visits candidates newest first
//! whatever they contain, so on text with many similar strings it spends
//! most of its budget on poor candidates.
//!
//! **Binary trees** (the LZMA SDK's BT4) keep each bucket as a search tree
//! ordered by the bytes *following* each position, like a dictionary.
//! Searching walks down the tree towards the current string: every step
//! either extends the best match or rules out a whole subtree, so the
//! budget goes to the most similar strings. The price is that every
//! position, even one skipped by the parser, must be inserted with a walk.
//! Insertion makes the new position the root: the walk splits the old
//! tree into the part that sorts before it (its left subtree) and after it
//! (its right subtree).

/// Longest match LZMA can code.
pub const MAX_LEN: usize = super::MATCH_MAX_LEN;

/// No position (positions are stored +1).
const NONE: u32 = 0;

const HASH3_BITS: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub len: u32,
    /// 1-based: 1 means the previous byte.
    pub dist: u32,
}

/// How positions with the same 4-byte hash are organised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    HashChain,
    BinaryTree,
}

pub struct MatchFinder<'a> {
    data: &'a [u8],
    kind: Kind,
    /// Largest distance allowed (the dictionary size).
    window: usize,
    /// Candidates to visit per search.
    depth: usize,
    /// Stop searching once a match this long is found.
    nice_len: usize,
    hash4_bits: u32,
    /// Latest position of each 2-byte string, indexed by its value.
    head2: Vec<u32>,
    /// Latest position of each hashed 3-byte string.
    head3: Vec<u32>,
    /// Latest position of each hashed 4-byte string: the chain start or
    /// tree root.
    head4: Vec<u32>,
    /// Positions kept, as a ring: position `p` lives at `p % cyc`.
    cyc: usize,
    /// Hash chains: the previous position in the bucket, one per slot.
    /// Binary trees: the left and right child, two per slot.
    links: Vec<u32>,
    /// Positions below this are in the tables.
    indexed: usize,
}

impl<'a> MatchFinder<'a> {
    pub fn new(data: &'a [u8], kind: Kind, window: usize, depth: usize, nice_len: usize) -> Self {
        assert!(
            data.len() < u32::MAX as usize,
            "input too large for 32-bit positions"
        );
        // Big enough for the input, no bigger: small inputs stay cheap.
        let hash4_bits = (usize::BITS - data.len().leading_zeros()).clamp(10, 20);
        // Distances up to `window` must fit in the ring.
        let cyc = window.min(data.len()) + 1;
        let per_slot = if kind == Kind::BinaryTree { 2 } else { 1 };
        Self {
            data,
            kind,
            window,
            depth,
            nice_len: nice_len.min(MAX_LEN),
            hash4_bits,
            head2: vec![NONE; 1 << 16],
            head3: vec![NONE; 1 << HASH3_BITS],
            head4: vec![NONE; 1 << hash4_bits],
            cyc,
            links: vec![NONE; cyc * per_slot],
            indexed: 0,
        }
    }

    fn key2(&self, pos: usize) -> usize {
        usize::from(self.data[pos]) | usize::from(self.data[pos + 1]) << 8
    }

    fn key3(&self, pos: usize) -> usize {
        let d = self.data;
        let v = u32::from(d[pos]) | u32::from(d[pos + 1]) << 8 | u32::from(d[pos + 2]) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH3_BITS)) as usize
    }

    fn key4(&self, pos: usize) -> usize {
        let v = u32::from_le_bytes(self.data[pos..pos + 4].try_into().expect("4 bytes"));
        (v.wrapping_mul(0x9E37_79B1) >> (32 - self.hash4_bits)) as usize
    }

    /// Add every position before `end` to the tables.
    fn index_until(&mut self, end: usize) {
        while self.indexed < end {
            let pos = self.indexed;
            match self.kind {
                Kind::HashChain => self.chain_insert(pos),
                Kind::BinaryTree => {
                    let root = self.take_root(pos);
                    let limit = self.tree_limit(pos);
                    self.tree_walk(pos, root, limit, 0, None);
                }
            }
            self.update_short_heads(pos);
            self.indexed += 1;
        }
    }

    fn update_short_heads(&mut self, pos: usize) {
        let n = self.data.len();
        let tag = pos as u32 + 1;
        if pos + 3 <= n {
            let h = self.key3(pos);
            self.head3[h] = tag;
        }
        if pos + 2 <= n {
            let h = self.key2(pos);
            self.head2[h] = tag;
        }
    }

    fn chain_insert(&mut self, pos: usize) {
        if pos + 4 <= self.data.len() {
            let h = self.key4(pos);
            self.links[pos % self.cyc] = self.head4[h];
            self.head4[h] = pos as u32 + 1;
        }
    }

    /// Make `pos` the root of its tree and return the old root, or `NONE`
    /// if `pos` is too close to the end to go in a tree.
    fn take_root(&mut self, pos: usize) -> u32 {
        if pos + 4 > self.data.len() {
            return NONE;
        }
        let h = self.key4(pos);
        std::mem::replace(&mut self.head4[h], pos as u32 + 1)
    }

    /// How far tree comparisons go at `pos`.
    fn tree_limit(&self, pos: usize) -> usize {
        self.nice_len.min(self.data.len() - pos)
    }

    /// Walk the tree under `root` towards the string at `pos`, rebuilding
    /// it with `pos` as the root. Matches longer than `best` go to `out`.
    /// Returns the longest length seen.
    fn tree_walk(
        &mut self,
        pos: usize,
        mut candidate: u32,
        limit: usize,
        mut best: usize,
        mut out: Option<&mut Vec<Match>>,
    ) -> usize {
        if pos + 4 > self.data.len() {
            return best;
        }
        let data = self.data;
        let slot = 2 * (pos % self.cyc);
        // Where to hang the next candidate that sorts after / before `pos`.
        // They start as `pos`'s own right and left child.
        let (mut after_link, mut before_link) = (slot + 1, slot);
        // Bytes known to agree with `pos` for everything reachable through
        // each side, so comparisons can start there.
        let (mut after_len, mut before_len) = (0, 0);
        for _ in 0..self.depth {
            if candidate == NONE {
                break;
            }
            let c = candidate as usize - 1;
            let dist = pos - c;
            if dist > self.window {
                break;
            }
            let pair = 2 * (c % self.cyc);
            let mut len = after_len.min(before_len);
            if data[c + len] == data[pos + len] {
                len += 1 + match_len(data, c + len + 1, pos + len + 1, limit - len - 1);
                if len > best {
                    best = len;
                    if let Some(out) = out.as_deref_mut() {
                        out.push(Match {
                            len: len as u32,
                            dist: dist as u32,
                        });
                    }
                }
                if len == limit {
                    // Identical as far as we compare: `pos` takes over the
                    // candidate's subtrees, and the candidate drops out.
                    self.links[before_link] = self.links[pair];
                    self.links[after_link] = self.links[pair + 1];
                    return best;
                }
            }
            if data[c + len] < data[pos + len] {
                // The candidate sorts before `pos`: it and its left subtree
                // go left; continue into its right subtree.
                self.links[before_link] = candidate;
                before_link = pair + 1;
                candidate = self.links[before_link];
                before_len = len;
            } else {
                self.links[after_link] = candidate;
                after_link = pair;
                candidate = self.links[after_link];
                after_len = len;
            }
        }
        // Out of budget or candidates: cut the rest off.
        self.links[after_link] = NONE;
        self.links[before_link] = NONE;
        best
    }

    /// Matches for `pos` in order of increasing length, written to `out`
    /// (cleared first). Each is the closest match found of its length.
    ///
    /// Each position may be searched at most once, in increasing order;
    /// positions skipped over are indexed without searching.
    pub fn find(&mut self, pos: usize, out: &mut Vec<Match>) {
        out.clear();
        debug_assert!(pos >= self.indexed, "position {pos} searched twice");
        self.index_until(pos);
        let data = self.data;
        let max_len = MAX_LEN.min(data.len() - pos);
        let mut best = 1;
        if max_len >= 2 {
            let short = [
                self.head2[self.key2(pos)],
                if max_len >= 3 {
                    self.head3[self.key3(pos)]
                } else {
                    NONE
                },
            ];
            for (i, &c) in short.iter().enumerate() {
                if c == NONE || (i == 1 && c == short[0]) {
                    continue;
                }
                let c = c as usize - 1;
                let dist = pos - c;
                if dist <= self.window && data[c + best] == data[pos + best] {
                    let len = match_len(data, c, pos, max_len);
                    if len > best {
                        best = len;
                        out.push(Match {
                            len: len as u32,
                            dist: dist as u32,
                        });
                    }
                }
            }
        }
        match self.kind {
            Kind::HashChain => {
                self.chain_search(pos, max_len, best, out);
                self.chain_insert(pos);
            }
            Kind::BinaryTree => {
                let root = self.take_root(pos);
                let limit = self.tree_limit(pos);
                // The tree must take `pos` even when a short match already
                // reaches the limit; it just records nothing then.
                let record = (best < limit).then_some(&mut *out);
                self.tree_walk(pos, root, limit, best, record);
                // Comparisons stop at nice_len; extend the longest match
                // to its full length.
                if let Some(last) = out.last_mut() {
                    if last.len as usize == limit && limit < max_len {
                        let d = last.dist as usize;
                        last.len = match_len(data, pos - d, pos, max_len) as u32;
                    }
                }
            }
        }
        self.update_short_heads(pos);
        self.indexed = pos + 1;
    }

    fn chain_search(&self, pos: usize, max_len: usize, mut best: usize, out: &mut Vec<Match>) {
        if max_len < 4 || best >= self.nice_len {
            return;
        }
        let data = self.data;
        let mut candidate = self.head4[self.key4(pos)];
        for _ in 0..self.depth {
            if candidate == NONE || best >= self.nice_len || best == max_len {
                break;
            }
            let c = candidate as usize - 1;
            let dist = pos - c;
            if dist > self.window {
                break;
            }
            if data[c + best] == data[pos + best] {
                let len = match_len(data, c, pos, max_len);
                if len > best {
                    best = len;
                    out.push(Match {
                        len: len as u32,
                        dist: dist as u32,
                    });
                }
            }
            let next = self.links[c % self.cyc];
            if next >= candidate {
                break; // link overwritten by a newer position
            }
            candidate = next;
        }
    }

    /// Length of the match at `pos` against distance `dist` (1-based), up
    /// to `limit`.
    pub fn len_at(&self, pos: usize, dist: usize, limit: usize) -> usize {
        if dist > pos {
            return 0;
        }
        match_len(self.data, pos - dist, pos, limit.min(self.data.len() - pos))
    }
}

/// How many bytes at `a` and `b` agree, up to `limit`. Needs `a < b` and
/// `b + limit <= data.len()`.
fn match_len(data: &[u8], a: usize, b: usize, limit: usize) -> usize {
    let mut len = 0;
    // 8 bytes at a time: the first differing bit says how many agree.
    while len + 8 <= limit {
        let x = u64::from_le_bytes(data[a + len..a + len + 8].try_into().expect("8 bytes"));
        let y = u64::from_le_bytes(data[b + len..b + len + 8].try_into().expect("8 bytes"));
        let diff = x ^ y;
        if diff != 0 {
            return len + (diff.trailing_zeros() / 8) as usize;
        }
        len += 8;
    }
    while len < limit && data[a + len] == data[b + len] {
        len += 1;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [Kind; 2] = [Kind::HashChain, Kind::BinaryTree];

    #[test]
    fn match_len_counts_agreeing_bytes() {
        let data = b"abcdefghijklmnopabcdefghijklmnoXabc";
        assert_eq!(match_len(data, 0, 16, 19), 15);
        assert_eq!(match_len(data, 0, 16, 7), 7);
        assert_eq!(match_len(data, 0, 32, 3), 3);
    }

    #[test]
    fn finds_increasing_matches_and_prefers_close_ones() {
        // "ab" close by, "abcdef" further back.
        let data = b"abcdef.......xab..abcdefgh";
        for kind in KINDS {
            let mut mf = MatchFinder::new(data, kind, 1 << 20, 16, 273);
            let mut out = Vec::new();
            mf.find(18, &mut out);
            assert_eq!(
                out,
                [Match { len: 2, dist: 4 }, Match { len: 6, dist: 18 }],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn respects_the_window() {
        let mut data = b"0123456789".to_vec();
        data.extend(std::iter::repeat_n(b'-', 100));
        data.extend(b"0123456789");
        for kind in KINDS {
            let mut out = Vec::new();
            MatchFinder::new(&data, kind, 64, 16, 273).find(110, &mut out);
            assert!(out.is_empty(), "{kind:?} {out:?}");
            MatchFinder::new(&data, kind, 128, 16, 273).find(110, &mut out);
            assert_eq!(out.last(), Some(&Match { len: 10, dist: 110 }), "{kind:?}");
        }
    }

    /// The longest match at each position by brute force, within `window`.
    fn brute_longest(data: &[u8], pos: usize, window: usize) -> usize {
        let max_len = MAX_LEN.min(data.len() - pos);
        (pos.saturating_sub(window)..pos)
            .map(|c| match_len(data, c, pos, max_len))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn binary_tree_finds_the_longest_match() {
        // With a generous budget the tree search is exact: at every
        // position, searched or skipped, it must see the longest match.
        let mut data = Vec::new();
        let mut s = 7u32;
        for _ in 0..6000 {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            // Few distinct symbols, so there are many partial matches.
            data.push(b"abcab"[(s >> 16) as usize % 5]);
        }
        for step in [1, 3] {
            let mut mf = MatchFinder::new(&data, Kind::BinaryTree, 1000, 10_000, 273);
            let mut out = Vec::new();
            for pos in (0..data.len()).step_by(step) {
                mf.find(pos, &mut out);
                let found = out.last().map_or(0, |m| m.len as usize);
                let want = brute_longest(&data, pos, 1000);
                // Lengths of 1 aren't reported.
                assert_eq!(
                    found,
                    if want < 2 { 0 } else { want },
                    "pos {pos} step {step}"
                );
                if let Some(m) = out.last() {
                    let d = m.dist as usize;
                    assert_eq!(
                        match_len(&data, pos - d, pos, m.len as usize),
                        m.len as usize
                    );
                }
            }
        }
    }
}
