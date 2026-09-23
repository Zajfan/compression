//! Match finder for LZMA: hash chains over a window of up to 4 GB.
//!
//! Like [`crate::lz77`]'s finder, but tuned for LZMA:
//!
//! - Matches can be as short as 2 bytes (worth it at small distances, since
//!   LZMA codes them cheaply), so the most recent position of each 2-byte
//!   and 3-byte string is kept in its own small table.
//! - Longer matches are found by walking a chain of earlier positions
//!   whose next 4 bytes hash the same.
//! - Every improvement found along the way is reported, not just the best,
//!   so the encoder can prefer a slightly shorter match that is much closer.

/// Longest match LZMA can code.
pub const MAX_LEN: usize = super::MATCH_MAX_LEN;

/// No position (positions are stored +1).
const NONE: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub len: u32,
    /// 1-based: 1 means the previous byte.
    pub dist: u32,
}

pub struct MatchFinder<'a> {
    data: &'a [u8],
    /// Largest distance allowed (the dictionary size).
    window: usize,
    /// Chain links to follow per search.
    depth: usize,
    /// Stop searching once a match this long is found.
    nice_len: usize,
    hash4_bits: u32,
    /// Latest position of each 2-byte string, indexed by its value.
    head2: Vec<u32>,
    /// Latest position of each hashed 3-byte string.
    head3: Vec<u32>,
    /// Latest position of each hashed 4-byte string: the chain start.
    head4: Vec<u32>,
    /// Previous position with the same 4-byte hash, at `pos % chain.len()`.
    chain: Vec<u32>,
    /// Positions below this are in the tables.
    indexed: usize,
}

const HASH3_BITS: u32 = 16;

impl<'a> MatchFinder<'a> {
    pub fn new(data: &'a [u8], window: usize, depth: usize, nice_len: usize) -> Self {
        assert!(
            data.len() < u32::MAX as usize,
            "input too large for 32-bit positions"
        );
        // Big enough for the input, no bigger: small inputs stay cheap.
        let hash4_bits = (usize::BITS - data.len().leading_zeros()).clamp(10, 20);
        let chain_len = window.min(data.len()).max(1);
        Self {
            data,
            window,
            depth,
            nice_len: nice_len.min(MAX_LEN),
            hash4_bits,
            head2: vec![NONE; 1 << 16],
            head3: vec![NONE; 1 << HASH3_BITS],
            head4: vec![NONE; 1 << hash4_bits],
            chain: vec![NONE; chain_len],
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
        let n = self.data.len();
        while self.indexed < end {
            let pos = self.indexed;
            let tag = pos as u32 + 1;
            if pos + 4 <= n {
                let h = self.key4(pos);
                let slot = pos % self.chain.len();
                self.chain[slot] = self.head4[h];
                self.head4[h] = tag;
            }
            if pos + 3 <= n {
                let h = self.key3(pos);
                self.head3[h] = tag;
            }
            if pos + 2 <= n {
                let h = self.key2(pos);
                self.head2[h] = tag;
            }
            self.indexed += 1;
        }
    }

    /// Matches for `pos` in order of increasing length, written to `out`
    /// (cleared first). Each is the closest match found of its length.
    pub fn find(&mut self, pos: usize, out: &mut Vec<Match>) {
        out.clear();
        self.index_until(pos);
        let data = self.data;
        let max_len = MAX_LEN.min(data.len() - pos);
        if max_len < 2 {
            return;
        }
        let mut best = 1;
        let consider = |candidate: u32, best: &mut usize, out: &mut Vec<Match>| -> bool {
            let c = candidate as usize - 1;
            let dist = pos - c;
            if dist > self.window {
                return false;
            }
            if *best < max_len && data[c + *best] == data[pos + *best] {
                let len = match_len(data, c, pos, max_len);
                if len > *best {
                    *best = len;
                    out.push(Match {
                        len: len as u32,
                        dist: dist as u32,
                    });
                }
            }
            true
        };

        let c2 = self.head2[self.key2(pos)];
        if c2 != NONE {
            consider(c2, &mut best, out);
        }
        if max_len >= 3 {
            let c3 = self.head3[self.key3(pos)];
            if c3 != NONE && c3 != c2 {
                consider(c3, &mut best, out);
            }
        }
        if max_len < 4 || best >= self.nice_len {
            return;
        }
        let mut candidate = self.head4[self.key4(pos)];
        for _ in 0..self.depth {
            if candidate == NONE || best >= self.nice_len || best == max_len {
                break;
            }
            if !consider(candidate, &mut best, out) {
                break;
            }
            let c = candidate as usize - 1;
            let next = self.chain[c % self.chain.len()];
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
        let mut mf = MatchFinder::new(data, 1 << 20, 16, 273);
        let mut out = Vec::new();
        mf.find(18, &mut out);
        assert_eq!(out, [Match { len: 2, dist: 4 }, Match { len: 6, dist: 18 }]);
    }

    #[test]
    fn respects_the_window() {
        let mut data = b"0123456789".to_vec();
        data.extend(std::iter::repeat_n(b'-', 100));
        data.extend(b"0123456789");
        let mut out = Vec::new();
        MatchFinder::new(&data, 64, 16, 273).find(110, &mut out);
        assert!(out.is_empty(), "{out:?}");
        MatchFinder::new(&data, 128, 16, 273).find(110, &mut out);
        assert_eq!(out.last(), Some(&Match { len: 10, dist: 110 }));
    }
}
