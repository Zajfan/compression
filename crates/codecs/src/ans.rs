//! rANS: range Asymmetric Numeral Systems, the entropy coder family behind
//! zstd, LZFSE, JPEG XL and AV1.
//!
//! ANS gets the fractional-bit efficiency of arithmetic coding with the
//! speed of Huffman. The whole coder state is one integer `x`. Coding a
//! symbol `s` that has frequency `f` (out of `M = 2^SCALE_BITS`) grows `x`
//! by a factor of about `M / f`, so it gains `log2(M / f)` bits, which is
//! exactly the symbol's information content. When `x` gets too big, its low
//! 16 bits are written out. (Writing 16 bits at a time rather than bytes
//! means a decode step needs at most one refill, so it can be done without
//! a hard-to-predict branch. That made decoding 60% faster.)
//!
//! ```text
//! encode: x' = (x / f) * M + start + (x % f)
//! decode: slot = x' % M        -> the symbol whose [start, start + f) holds slot
//!         x    = f * (x' / M) + slot - start
//! ```
//!
//! Decoding is the exact inverse of encoding, so it runs *backwards*: the
//! last symbol encoded is the first decoded. The encoder therefore walks
//! the input from the end, and its output is reversed so the decoder can
//! read front to back.
//!
//! Decoding a symbol is a table lookup, a multiply and an add, with no bit
//! loop. And unlike a range coder, several independent states can be
//! interleaved in one stream ([`LANES`]), so the CPU can work on several
//! symbols at once. Both are why ANS decodes several times faster than the
//! bitwise range coder in [`crate::range`].
//!
//! Frequencies are static here: counted in advance and sent with the data.
//!
//! Reference: Fabian Giesen's `ryg_rans` (public domain), and Jarek Duda,
//! "Asymmetric numeral systems" (2009).

use crate::{Error, Result};

/// Frequencies are scaled to sum to `M = 1 << SCALE_BITS`.
pub const SCALE_BITS: u32 = 12;
const M: u32 = 1 << SCALE_BITS;
/// Lower bound of the normalized state; `x` stays in `[L, 2^32)`.
const L: u32 = 1 << 16;
/// Independent states interleaved in one stream. Symbol `i` uses state
/// `i % LANES`.
pub const LANES: usize = 4;

/// Normalized symbol frequencies for bytes, summing to `M`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freqs {
    freq: [u32; 256],
    /// Cumulative frequency: symbol `s` owns slots `start[s]..start[s] + freq[s]`.
    start: [u32; 256],
}

impl Freqs {
    /// Scale byte counts to frequencies summing to `M`. Every byte that
    /// occurs keeps at least frequency 1, since frequency 0 can't be coded.
    /// Returns `None` when all counts are 0.
    pub fn normalize(counts: &[u64; 256]) -> Option<Self> {
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return None;
        }
        let mut freq = [0u32; 256];
        for (f, &c) in freq.iter_mut().zip(counts) {
            if c > 0 {
                *f = ((c * u64::from(M) + total / 2) / total).max(1) as u32;
            }
        }
        // Rounding leaves the sum a little off. Fix it on the most common
        // symbols, where one step changes the cost least.
        let mut sum: u32 = freq.iter().sum();
        while sum != M {
            let (i, _) = freq
                .iter()
                .enumerate()
                .filter(|&(_, &f)| sum < M || f > 1)
                .max_by_key(|&(_, &f)| f)
                .expect("some symbol can be adjusted");
            if sum < M {
                freq[i] += 1;
                sum += 1;
            } else {
                freq[i] -= 1;
                sum -= 1;
            }
        }
        Some(Self::from_freqs(freq).expect("normalized frequencies sum to M"))
    }

    /// Frequencies as stored in a stream. They must sum to exactly `M`.
    pub fn from_freqs(freq: [u32; 256]) -> Result<Self> {
        let mut start = [0u32; 256];
        let mut sum = 0u32;
        for (st, &f) in start.iter_mut().zip(&freq) {
            *st = sum;
            sum = sum
                .checked_add(f)
                .filter(|&s| s <= M)
                .ok_or(Error::Corrupt("rANS frequencies exceed the total"))?;
        }
        if sum != M {
            return Err(Error::Corrupt("rANS frequencies do not add up"));
        }
        Ok(Self { freq, start })
    }

    pub fn freqs(&self) -> &[u32; 256] {
        &self.freq
    }
}

/// Decoding table entry for one slot of `0..M`.
#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    symbol: u8,
    freq: u16,
    /// `slot - start[symbol]`.
    bias: u16,
}

/// Encode `data`. Every byte in it must have a nonzero frequency in `f`.
pub fn encode(data: &[u8], f: &Freqs) -> Vec<u8> {
    let mut words: Vec<u16> = Vec::with_capacity(data.len() / 4);
    let mut x = [L; LANES];
    // Backwards, so the decoder gets the symbols front to back.
    for (i, &b) in data.iter().enumerate().rev() {
        let s = usize::from(b);
        let (freq, start) = (f.freq[s], f.start[s]);
        debug_assert!(freq > 0, "byte {b} has no frequency");
        let state = &mut x[i % LANES];
        // Shrink x so coding s keeps it below 2^32. Once is always enough.
        // (In u64: for a symbol with frequency M this is exactly 2^32.)
        let x_max = (u64::from(L >> SCALE_BITS) << 16) * u64::from(freq);
        if u64::from(*state) >= x_max {
            words.push(*state as u16);
            *state >>= 16;
        }
        *state = ((*state / freq) << SCALE_BITS) + *state % freq + start;
    }
    // Final states first, lane 0 first, then the words in reverse order of
    // writing, which is the order the decoder needs them.
    let mut out = Vec::with_capacity(4 * LANES + 2 * words.len());
    for state in x {
        out.extend_from_slice(&state.to_le_bytes());
    }
    for w in words.iter().rev() {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Decode `n` bytes from `input` into `out`. `input` must be exactly one
/// stream from [`encode`] with the same frequencies.
pub fn decode(input: &[u8], f: &Freqs, n: usize, out: &mut Vec<u8>) -> Result<()> {
    let head = input
        .get(..4 * LANES)
        .ok_or(Error::Corrupt("rANS stream too short"))?;
    let mut x = [0u32; LANES];
    for (state, bytes) in x.iter_mut().zip(head.chunks_exact(4)) {
        *state = u32::from_le_bytes(bytes.try_into().expect("4 bytes"));
        if *state < L {
            return Err(Error::Corrupt("rANS state out of range"));
        }
    }

    // One lookup per symbol: everything decoding needs, by slot.
    let mut table = vec![Slot::default(); M as usize];
    for s in 0..256 {
        let (start, freq) = (f.start[s], f.freq[s]);
        for slot in start..start + freq {
            table[slot as usize] = Slot {
                symbol: s as u8,
                freq: freq as u16,
                bias: (slot - start) as u16,
            };
        }
    }

    let mut pos = 4 * LANES;
    let first = out.len();
    out.resize(first + n, 0);
    let dest = &mut out[first..];
    let mut step = |state: &mut u32, byte: &mut u8| {
        let e = table[(*state & (M - 1)) as usize];
        *byte = e.symbol;
        *state = u32::from(e.freq) * (*state >> SCALE_BITS) + u32::from(e.bias);
        // Refill 16 bits if the state dropped below L, without branching:
        // always load the next word, and use it only when needed.
        // Past the end reads as 0; the checks below catch it.
        let need = *state < L;
        let word = match input.get(pos..pos + 2) {
            Some(w) => u32::from(u16::from_le_bytes([w[0], w[1]])),
            None => 0,
        };
        let shift = 16 * u32::from(need);
        *state = (*state << shift) | (word & ((1 << shift) - 1));
        pos += 2 * usize::from(need);
    };
    // Whole rounds of LANES symbols, so the lanes' independent work can
    // overlap in the CPU, then the leftover symbols.
    let mut rounds = dest.chunks_exact_mut(LANES);
    for round in &mut rounds {
        for (state, byte) in x.iter_mut().zip(round) {
            step(state, byte);
        }
    }
    for (state, byte) in x.iter_mut().zip(rounds.into_remainder()) {
        step(state, byte);
    }
    // The encoder started every state at L, so a clean decode ends there.
    // This catches truncation and most damage, but it is not a checksum:
    // ANS states resynchronize, so some flipped bits decode to wrong bytes
    // and still end at L. The frame's CRC-32 is what guarantees integrity.
    if pos != input.len() || x.iter().any(|&state| state != L) {
        return Err(Error::Corrupt("rANS stream did not end cleanly"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(data: &[u8]) -> [u64; 256] {
        let mut c = [0u64; 256];
        for &b in data {
            c[usize::from(b)] += 1;
        }
        c
    }

    fn roundtrip(data: &[u8]) -> Vec<u8> {
        let f = Freqs::normalize(&counts(data)).unwrap();
        let packed = encode(data, &f);
        let mut out = Vec::new();
        decode(&packed, &f, data.len(), &mut out).unwrap();
        assert_eq!(out, data);
        packed
    }

    #[test]
    fn normalize_sums_to_m_and_keeps_rare_symbols() {
        let mut c = [0u64; 256];
        c[0] = 1_000_000;
        for x in &mut c[1..] {
            *x = 1;
        }
        let f = Freqs::normalize(&c).unwrap();
        assert_eq!(f.freqs().iter().sum::<u32>(), M);
        assert!(f.freqs().iter().all(|&x| x >= 1));
        assert_eq!(f.freqs()[0], M - 255);
        assert_eq!(Freqs::normalize(&[0; 256]), None);
    }

    #[test]
    fn roundtrips() {
        roundtrip(b"a");
        roundtrip(b"abc");
        roundtrip(&[9; 10_000]);
        roundtrip(&(0..=255).cycle().take(5000).collect::<Vec<u8>>());
        roundtrip(&b"it was the best of times, it was the worst of times".repeat(100));
    }

    #[test]
    fn a_single_symbol_costs_nothing() {
        // Frequency M means probability 1: the state never changes.
        assert_eq!(roundtrip(&[42; 100_000]).len(), 4 * LANES);
    }

    #[test]
    fn close_to_entropy() {
        let text = b"it was the best of times, it was the worst of times".repeat(1000);
        let ideal = crate::stats::entropy_bits_per_byte(&text) * text.len() as f64 / 8.0;
        let packed = roundtrip(&text).len() as f64;
        assert!(packed < ideal * 1.01, "{packed} vs {ideal}");
    }

    #[test]
    fn corruption_is_detected() {
        let data = b"hello rANS, hello world".repeat(20);
        let f = Freqs::normalize(&counts(&data)).unwrap();
        let packed = encode(&data, &f);
        let mut out = Vec::new();
        assert!(decode(&packed[..packed.len() - 1], &f, data.len(), &mut out).is_err());
        out.clear();
        assert!(decode(&packed, &f, data.len() + 1, &mut out).is_err());
        assert!(decode(&[0xFF; 16], &f, 1, &mut out).is_err());
        let mut bad = *f.freqs();
        bad[0] += 1;
        assert!(Freqs::from_freqs(bad).is_err());
    }
}
