//! Huffman coding: short codes for common symbols, long codes for rare ones.
//!
//! Three steps, each reusable by later codecs (Deflate uses all of them):
//!
//! 1. [`code_lengths`]: from symbol counts, decide how many bits each
//!    symbol gets. This is the actual Huffman algorithm, plus a length limit.
//! 2. [`canonical_codes`]: turn lengths into concrete bit patterns. With
//!    "canonical" codes the lengths alone define the codes, so an archive
//!    stores only the lengths.
//! 3. [`Encoder`] / [`Decoder`]: write and read symbols using those codes.
//!    The decoder is table-driven: one lookup per symbol, no tree walking.

use crate::bits::{BitReader, BitWriter};
use crate::{Error, Result};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Longest code allowed. Deflate uses 15, and so do we, so our tables are
/// Deflate-compatible and the decode table stays small (2^15 entries).
pub const MAX_CODE_LEN: u32 = 15;

/// Compute a code length for every symbol from how often it occurs.
///
/// Symbols with count 0 get length 0 (no code). No length exceeds
/// `max_len`. Among codes obeying that limit, the total encoded size
/// `Σ count × length` is close to minimal: optimal when the limit does not
/// bite, and a well-known good approximation (the one miniz and zlib-like
/// encoders use) when it does.
pub fn code_lengths(counts: &[u64], max_len: u32) -> Vec<u8> {
    assert!(max_len <= MAX_CODE_LEN);
    let mut lengths = vec![0u8; counts.len()];
    let used: Vec<usize> = (0..counts.len()).filter(|&s| counts[s] > 0).collect();
    assert!(used.len() <= 1 << max_len, "too many symbols for max_len");
    match used.len() {
        0 => return lengths,
        // A single symbol still needs one bit per occurrence so the decoder
        // can count them.
        1 => {
            lengths[used[0]] = 1;
            return lengths;
        }
        _ => {}
    }

    // Classic Huffman: repeatedly merge the two lightest nodes into a new
    // node. Leaves are nodes 0..n, merged nodes are appended after them, so
    // every parent has a larger index than its children.
    let n = used.len();
    let mut parent = vec![0usize; 2 * n - 1];
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = used
        .iter()
        .enumerate()
        .map(|(node, &sym)| Reverse((counts[sym], node)))
        .collect();
    let mut next = n;
    while let (Some(Reverse((wa, a))), Some(Reverse((wb, b)))) = (heap.pop(), heap.pop()) {
        parent[a] = next;
        parent[b] = next;
        heap.push(Reverse((wa + wb, next)));
        next += 1;
    }

    // A leaf's depth in the tree is its code length. Walk from the root
    // (the last node) down: every node's depth is its parent's plus one.
    let root = next - 1;
    let mut depth = vec![0u32; 2 * n - 1];
    for node in (0..root).rev() {
        depth[node] = depth[parent[node]] + 1;
    }

    // How many codes of each length the tree produced.
    let tree_max = (0..n).map(|leaf| depth[leaf]).max().unwrap_or(0);
    let mut per_len = vec![0u32; tree_max.max(max_len) as usize + 1];
    for leaf in 0..n {
        per_len[depth[leaf] as usize] += 1;
    }
    limit_lengths(&mut per_len, max_len);

    // Hand out lengths: most frequent symbols get the shortest codes.
    let mut by_count = used;
    by_count.sort_by_key(|&s| (Reverse(counts[s]), s));
    let mut symbols = by_count.into_iter();
    for (len, &how_many) in per_len.iter().enumerate().take(max_len as usize + 1) {
        for sym in symbols.by_ref().take(how_many as usize) {
            lengths[sym] = len as u8;
        }
    }
    lengths
}

/// Squash codes longer than `max_len` while keeping the code valid.
///
/// Every code of length `l` uses up `2^(max_len - l)` of the available
/// `2^max_len` "slots" (the Kraft inequality). Clamping long codes to
/// `max_len` over-fills the slots, so we repeatedly remove one max-length
/// code and split a shorter leaf into two, which frees exactly one slot,
/// until the slots are exactly full again.
fn limit_lengths(per_len: &mut [u32], max_len: u32) {
    let max = max_len as usize;
    let overflow: u32 = per_len[max + 1..].iter().sum();
    if overflow == 0 {
        return;
    }
    per_len[max] += overflow;
    per_len[max + 1..].fill(0);
    let mut total: u64 = (1..=max).map(|l| u64::from(per_len[l]) << (max - l)).sum();
    while total > 1 << max {
        per_len[max] -= 1;
        if let Some(l) = (1..max).rev().find(|&l| per_len[l] > 0) {
            per_len[l] -= 1;
            per_len[l + 1] += 2;
        }
        total -= 1;
    }
}

/// Assign canonical codes from lengths (the algorithm in RFC 1951 §3.2.2).
///
/// Codes of the same length are consecutive numbers in symbol order, and
/// shorter codes come before longer ones. Returned codes are plain numbers,
/// to be sent most significant bit first. Symbols with length 0 get 0.
pub fn canonical_codes(lengths: &[u8]) -> Vec<u32> {
    let max = lengths.iter().copied().max().unwrap_or(0) as usize;
    let mut per_len = vec![0u32; max + 1];
    for &l in lengths.iter().filter(|&&l| l > 0) {
        per_len[l as usize] += 1;
    }
    let mut next_code = vec![0u32; max + 2];
    let mut code = 0;
    for len in 1..=max {
        code = (code + per_len[len - 1]) << 1;
        next_code[len] = code;
    }
    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next_code[l as usize];
            next_code[l as usize] += 1;
            c
        })
        .collect()
}

/// Reverse the low `len` bits of `code`.
///
/// Huffman codes are defined most-significant-bit first, but our bit stream
/// is LSB-first (like Deflate), so codes are stored reversed.
fn reverse_bits(code: u32, len: u8) -> u32 {
    if len == 0 {
        0
    } else {
        code.reverse_bits() >> (32 - u32::from(len))
    }
}

/// Check the lengths describe a usable prefix code (Kraft sum ≤ 1).
fn validate(lengths: &[u8]) -> Result<()> {
    let mut slots: u64 = 0;
    for &l in lengths {
        if u32::from(l) > MAX_CODE_LEN {
            return Err(Error::Corrupt("huffman code length above 15"));
        }
        if l > 0 {
            slots += 1 << (MAX_CODE_LEN - u32::from(l));
        }
    }
    if slots > 1 << MAX_CODE_LEN {
        return Err(Error::Corrupt("huffman code lengths over-subscribed"));
    }
    Ok(())
}

/// Writes symbols with a fixed set of Huffman codes.
#[derive(Debug, Clone)]
pub struct Encoder {
    /// Per symbol: (bit-reversed code, length).
    codes: Vec<(u32, u8)>,
}

impl Encoder {
    pub fn from_lengths(lengths: &[u8]) -> Self {
        let codes = canonical_codes(lengths)
            .into_iter()
            .zip(lengths)
            .map(|(code, &len)| (reverse_bits(code, len), len))
            .collect();
        Self { codes }
    }

    /// Write `symbol`. Panics in debug builds if it has no code.
    pub fn write(&self, w: &mut BitWriter, symbol: usize) {
        let (code, len) = self.codes[symbol];
        debug_assert!(len > 0, "symbol {symbol} has no code");
        w.write_bits(code, u32::from(len));
    }

    /// Bits needed to encode `symbol`.
    pub fn len(&self, symbol: usize) -> u8 {
        self.codes[symbol].1
    }
}

/// Reads symbols with a lookup table.
///
/// The table has one entry for every possible `max_len`-bit input. For a
/// symbol with a `len`-bit code, every entry whose low `len` bits equal the
/// (reversed) code points to it, since the remaining bits belong to the
/// following symbols. Decoding is then: peek `max_len` bits, look up, consume
/// `len` bits.
#[derive(Debug, Clone)]
pub struct Decoder {
    /// `symbol << 4 | len`; `len == 0` marks bit patterns that are not a code.
    table: Vec<u16>,
    bits: u32,
}

impl Decoder {
    pub fn from_lengths(lengths: &[u8]) -> Result<Self> {
        validate(lengths)?;
        assert!(lengths.len() <= 1 << 12, "symbol must fit in 12 bits");
        let bits = u32::from(lengths.iter().copied().max().unwrap_or(0)).max(1);
        let size = 1usize << bits;
        let mut table = vec![0u16; size];
        for (sym, (code, &len)) in canonical_codes(lengths)
            .into_iter()
            .zip(lengths)
            .enumerate()
        {
            if len == 0 {
                continue;
            }
            let entry = (sym as u16) << 4 | u16::from(len);
            let step = 1usize << len;
            let mut i = reverse_bits(code, len) as usize;
            while i < size {
                table[i] = entry;
                i += step;
            }
        }
        Ok(Self { table, bits })
    }

    pub fn read(&self, r: &mut BitReader) -> Result<usize> {
        let entry = self.table[r.peek_bits(self.bits) as usize];
        let len = u32::from(entry & 0xF);
        if len == 0 {
            return Err(Error::Corrupt("invalid huffman code"));
        }
        r.consume(len)?;
        Ok(usize::from(entry >> 4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn kraft_sum(lengths: &[u8]) -> f64 {
        lengths
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 0.5f64.powi(l.into()))
            .sum()
    }

    #[test]
    fn textbook_example() {
        // a:5 b:2 c:1 d:1 → a=1 bit, b=2 bits, c and d=3 bits.
        assert_eq!(code_lengths(&[5, 2, 1, 1], 15), [1, 2, 3, 3]);
    }

    #[test]
    fn rfc1951_canonical_example() {
        // RFC 1951 §3.2.2: lengths (3,3,3,3,3,2,4,4) for symbols A..H.
        let codes = canonical_codes(&[3, 3, 3, 3, 3, 2, 4, 4]);
        assert_eq!(
            codes,
            [0b010, 0b011, 0b100, 0b101, 0b110, 0b00, 0b1110, 0b1111]
        );
    }

    #[test]
    fn length_limit_on_fibonacci_counts() {
        // Fibonacci counts make the deepest possible tree: 29 symbols would
        // need a 28-bit code without the limit.
        let mut counts = vec![1u64, 1];
        while counts.len() < 29 {
            let n = counts.len();
            counts.push(counts[n - 1] + counts[n - 2]);
        }
        let lengths = code_lengths(&counts, 15);
        assert!(lengths.iter().all(|&l| (1..=15).contains(&l)));
        assert!(
            (kraft_sum(&lengths) - 1.0).abs() < 1e-12,
            "code must be complete"
        );
    }

    #[test]
    fn single_and_no_symbols() {
        assert_eq!(code_lengths(&[0, 0, 7, 0], 15), [0, 0, 1, 0]);
        assert_eq!(code_lengths(&[0, 0], 15), [0, 0]);
    }

    #[test]
    fn decoder_rejects_oversubscribed() {
        assert!(Decoder::from_lengths(&[1, 1, 1]).is_err());
    }

    proptest! {
        #[test]
        fn lengths_are_valid_and_near_optimal(
            counts in proptest::collection::vec(0u64..100_000, 2..300),
            max_len in 9u32..=15,
        ) {
            let lengths = code_lengths(&counts, max_len);
            let used = counts.iter().filter(|&&c| c > 0).count();
            for (c, l) in counts.iter().zip(&lengths) {
                prop_assert_eq!(*c > 0, *l > 0);
                prop_assert!(u32::from(*l) <= max_len);
            }
            prop_assert!(kraft_sum(&lengths) <= 1.0 + 1e-12);
            if used >= 2 {
                prop_assert!((kraft_sum(&lengths) - 1.0).abs() < 1e-12, "code must be complete");
                // Shannon: a prefix code can't beat entropy, and Huffman is
                // within 1 bit per symbol of it (without a binding limit).
                let total: u64 = counts.iter().sum();
                let bits: u64 = counts.iter().zip(&lengths).map(|(&c, &l)| c * u64::from(l)).sum();
                let entropy: f64 = counts.iter().filter(|&&c| c > 0)
                    .map(|&c| { let p = c as f64 / total as f64; -(c as f64) * p.log2() }).sum();
                prop_assert!(bits as f64 >= entropy - 1e-6);
                if max_len == 15 && used <= 64 {
                    prop_assert!((bits as f64) < entropy + total as f64);
                }
            }
        }

        #[test]
        fn encode_decode_symbols(
            counts in proptest::collection::vec(0u64..1000, 2..300),
            picks in proptest::collection::vec(any::<prop::sample::Index>(), 0..500),
        ) {
            let lengths = code_lengths(&counts, 15);
            let symbols: Vec<usize> = (0..counts.len()).filter(|&s| lengths[s] > 0).collect();
            prop_assume!(!symbols.is_empty());
            let message: Vec<usize> = picks.iter().map(|i| *i.get(&symbols)).collect();
            let enc = Encoder::from_lengths(&lengths);
            let mut w = BitWriter::new();
            for &s in &message {
                enc.write(&mut w, s);
            }
            let bytes = w.finish();
            let dec = Decoder::from_lengths(&lengths).unwrap();
            let mut r = BitReader::new(&bytes);
            for &s in &message {
                prop_assert_eq!(dec.read(&mut r)?, s);
            }
        }
    }
}
