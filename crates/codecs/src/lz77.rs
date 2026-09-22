//! LZ77: replace repeated strings with "go back `dist` bytes, copy `len`".
//!
//! Huffman only exploits *which bytes* are common. LZ77 exploits *repeated
//! strings*: in `"the cat and the hat"`, the second `"the "` becomes
//! `Match { len: 4, dist: 12 }`. This is where most compression of real
//! files comes from.
//!
//! [`parse`] turns input into [`Token`]s. It uses Deflate's limits (32 KB
//! window, matches of 3 to 258 bytes) so Deflate can use its output
//! directly.
//!
//! ## Finding matches quickly: hash chains
//!
//! Checking every earlier position would be far too slow. Instead we hash
//! the next 3 bytes at every position. `head[h]` holds the most recent
//! position with hash `h`, and `prev[pos]` links each position to the
//! previous one with the same hash. Walking that chain visits only
//! positions that probably start with the same 3 bytes. `max_chain` caps
//! how many we try; that is the main speed/ratio dial (the "level").
//!
//! ## Lazy matching
//!
//! Taking the first match found (greedy) is not always best. In
//! `"abcd…bcdefgh"` a short match at `a` can hide a longer one starting at
//! `b`. With lazy matching we also look one byte ahead, and if the next
//! position has a longer match we emit a literal and take that one instead.
//! zlib does the same.

/// Shortest match worth encoding.
pub const MIN_MATCH: usize = 3;
/// Longest match Deflate can encode.
pub const MAX_MATCH: usize = 258;
/// How far back matches may reach.
pub const WINDOW: usize = 32 * 1024;

const HASH_BITS: u32 = 15;
const NONE: u32 = 0;

/// One step of the parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Literal(u8),
    /// Copy `len` bytes starting `dist` bytes back. `len` may exceed
    /// `dist`: the copy then overlaps itself, which is how a run like
    /// `"aaaaaaa"` becomes one literal plus `Match { len: 6, dist: 1 }`.
    Match {
        len: u16,
        dist: u16,
    },
}

impl Token {
    /// Number of input bytes this token stands for.
    pub fn byte_len(self) -> usize {
        match self {
            Token::Literal(_) => 1,
            Token::Match { len, .. } => usize::from(len),
        }
    }
}

/// Search effort. Higher levels try more candidates and compress better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Most candidates examined per search.
    pub max_chain: usize,
    /// Stop searching once a match this long is found.
    pub nice_len: usize,
    /// Only look for a better match one byte ahead if the current match is
    /// shorter than this (0 = greedy, never look ahead).
    pub lazy_len: usize,
    /// If the current match is already this long, look ahead with a quarter
    /// of the effort.
    pub good_len: usize,
}

impl Params {
    /// Presets for levels 1 (fastest) to 9 (smallest), modelled on zlib's.
    pub fn level(level: u8) -> Self {
        let (good_len, lazy_len, nice_len, max_chain) = match level {
            0 | 1 => (4, 0, 8, 4),
            2 => (4, 0, 16, 8),
            3 => (4, 0, 32, 32),
            4 => (4, 4, 16, 16),
            5 => (8, 16, 32, 32),
            6 => (8, 16, 128, 128),
            7 => (8, 32, 128, 256),
            8 => (32, 128, 258, 1024),
            _ => (32, 258, 258, 4096),
        };
        Self {
            max_chain,
            nice_len,
            lazy_len,
            good_len,
        }
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::level(6)
    }
}

/// Hash-chain index over the input.
struct MatchFinder<'a> {
    data: &'a [u8],
    /// Most recent position (+1, so 0 means none) for each hash.
    head: Vec<u32>,
    /// Previous position (+1) with the same hash, indexed by `pos % WINDOW`.
    /// Older links are overwritten, but they are out of reach by then.
    prev: Vec<u32>,
    /// Positions below this have been added to the index.
    indexed: usize,
}

impl<'a> MatchFinder<'a> {
    fn new(data: &'a [u8]) -> Self {
        assert!(
            data.len() < u32::MAX as usize,
            "input too large for 32-bit positions"
        );
        Self {
            data,
            head: vec![NONE; 1 << HASH_BITS],
            prev: vec![NONE; WINDOW],
            indexed: 0,
        }
    }

    fn hash(&self, pos: usize) -> usize {
        let d = self.data;
        let v = u32::from(d[pos]) << 16 | u32::from(d[pos + 1]) << 8 | u32::from(d[pos + 2]);
        // Multiplicative (Fibonacci) hashing spreads nearby values apart.
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    /// Add every position before `end` to the index.
    fn index_until(&mut self, end: usize) {
        let last_hashable = self.data.len().saturating_sub(MIN_MATCH - 1);
        while self.indexed < end.min(last_hashable) {
            let pos = self.indexed;
            let h = self.hash(pos);
            self.prev[pos % WINDOW] = self.head[h];
            self.head[h] = pos as u32 + 1;
            self.indexed += 1;
        }
        self.indexed = self.indexed.max(end);
    }

    /// Longest match for `pos` that is longer than `beat`, as `(len, dist)`.
    /// Returns `(beat, 0)` if there is none.
    fn find(
        &mut self,
        pos: usize,
        max_chain: usize,
        nice_len: usize,
        beat: usize,
    ) -> (usize, usize) {
        self.index_until(pos);
        let data = self.data;
        let max_len = MAX_MATCH.min(data.len() - pos);
        let (mut best_len, mut best_dist) = (beat, 0);
        if max_len < MIN_MATCH || best_len >= max_len {
            return (best_len, best_dist);
        }
        let mut candidate = self.head[self.hash(pos)];
        let mut chain = max_chain;
        while candidate != NONE && chain > 0 {
            let c = candidate as usize - 1;
            let dist = pos - c;
            if dist > WINDOW {
                break;
            }
            // Cheap early reject: a longer match must agree at `best_len`.
            if data[c + best_len] == data[pos + best_len] {
                let len = data[c..c + max_len]
                    .iter()
                    .zip(&data[pos..pos + max_len])
                    .take_while(|(a, b)| a == b)
                    .count();
                if len > best_len {
                    best_len = len;
                    best_dist = dist;
                    if len >= nice_len || len == max_len {
                        break;
                    }
                }
            }
            let next = self.prev[c % WINDOW];
            if next >= candidate {
                break; // link was overwritten by a newer position
            }
            candidate = next;
            chain -= 1;
        }
        (best_len, best_dist)
    }
}

/// Split `data` into literals and back-references.
pub fn parse(data: &[u8], params: Params) -> Vec<Token> {
    let mut finder = MatchFinder::new(data);
    let mut tokens = Vec::with_capacity(data.len() / 3);
    let mut pos = 0;
    // A match already found for `pos` by the previous lazy look-ahead.
    let mut pending = None;
    while pos < data.len() {
        let (len, dist) = pending
            .take()
            .unwrap_or_else(|| finder.find(pos, params.max_chain, params.nice_len, MIN_MATCH - 1));
        if len < MIN_MATCH {
            tokens.push(Token::Literal(data[pos]));
            pos += 1;
            continue;
        }
        if len < params.lazy_len && pos + 1 < data.len() {
            let effort = if len >= params.good_len {
                params.max_chain / 4
            } else {
                params.max_chain
            };
            let next = finder.find(pos + 1, effort.max(1), params.nice_len, len);
            if next.0 > len {
                tokens.push(Token::Literal(data[pos]));
                pos += 1;
                pending = Some(next);
                continue;
            }
        }
        tokens.push(Token::Match {
            len: len as u16,
            dist: dist as u16,
        });
        pos += len;
    }
    tokens
}

/// Rebuild the original bytes from tokens (the LZ77 decoder).
///
/// Panics on a match reaching before the start; decoders that read tokens
/// from untrusted input check that first.
pub fn reconstruct(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::new();
    for &t in tokens {
        match t {
            Token::Literal(b) => out.push(b),
            Token::Match { len, dist } => copy_match(&mut out, usize::from(dist), usize::from(len)),
        }
    }
    out
}

/// Append `len` bytes copied from `dist` bytes back. Byte by byte, because
/// source and destination may overlap (`len > dist`).
pub fn copy_match(out: &mut Vec<u8>, dist: usize, len: usize) {
    let start = out.len() - dist;
    if len <= dist {
        out.extend_from_within(start..start + len);
    } else {
        for i in 0..len {
            out.push(out[start + i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn check(data: &[u8], params: Params) -> Vec<Token> {
        let tokens = parse(data, params);
        for t in &tokens {
            if let Token::Match { len, dist } = *t {
                assert!((MIN_MATCH..=MAX_MATCH).contains(&usize::from(len)));
                assert!((1..=WINDOW).contains(&usize::from(dist)));
            }
        }
        assert_eq!(reconstruct(&tokens), data);
        tokens
    }

    #[test]
    fn finds_repeated_word() {
        let tokens = check(b"the cat and the hat", Params::default());
        assert!(
            tokens.contains(&Token::Match { len: 4, dist: 12 }),
            "{tokens:?}"
        );
    }

    #[test]
    fn run_becomes_overlapping_match() {
        let tokens = check(&[b'a'; 100], Params::default());
        assert_eq!(
            tokens,
            [Token::Literal(b'a'), Token::Match { len: 99, dist: 1 }]
        );
    }

    #[test]
    fn lazy_finds_longer_match() {
        // At the final "abcdefgh", greedy grabs "abc" (3 bytes) from the
        // start. Lazy looks one byte ahead, sees "bcdefgh" (7 bytes) and
        // takes that instead.
        let data = b"abc_bcdefgh_abcdefgh";
        let greedy = check(
            data,
            Params {
                lazy_len: 0,
                ..Params::default()
            },
        );
        let lazy = check(data, Params::default());
        assert!(
            greedy.contains(&Token::Match { len: 3, dist: 12 }),
            "{greedy:?}"
        );
        assert!(lazy.contains(&Token::Match { len: 7, dist: 9 }), "{lazy:?}");
    }

    #[test]
    fn respects_window() {
        let mut data = b"0123456789".to_vec();
        data.extend(vec![b'x'; WINDOW]);
        data.extend(b"0123456789");
        check(&data, Params::level(9));
    }

    proptest! {
        #[test]
        fn tokens_rebuild_input(data in proptest::collection::vec(0u8..4, 0..5000), level in 1u8..=9) {
            check(&data, Params::level(level));
        }
    }
}
