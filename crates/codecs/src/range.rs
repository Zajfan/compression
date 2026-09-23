//! Binary adaptive range coder, the entropy coder inside LZMA / 7-Zip.
//!
//! Huffman gives every symbol a whole number of bits, so a byte that is 99%
//! predictable still costs at least 1 bit. A range coder has no such limit:
//! it keeps an interval `[low, low + range)` and, for each bit, shrinks it
//! to the part that matches the bit's probability. Likely bits shrink it only
//! a little and cost a fraction of a bit; unlikely ones shrink it a lot. The
//! output is just enough digits of `low` to point inside the final interval.
//!
//! ```text
//!  range ├────────── bit = 0 (p) ──────────┼── bit = 1 ──┤
//!        low                             low + bound
//! ```
//!
//! Everything is coded one bit at a time. Each bit has its own [`Prob`], an
//! 11-bit estimate that the bit is 0, which moves 1/32 of the way towards
//! each bit seen. A byte is coded as 8 bits walking down a binary tree
//! ([`Encoder::encode_tree`]), so each tree node learns its own probability.
//!
//! The details (32-bit range, 11-bit probabilities, the carry-handling
//! `cache`) follow LZMA exactly, so the same coder serves the LZMA codec
//! later. Reference: `LzmaEnc.c` / `LzmaDec.c` in the public-domain LZMA SDK.

/// Probability that the next bit is 0, scaled so `PROB_ONE` means certain.
pub type Prob = u16;

const PROB_BITS: u32 = 11;
const PROB_ONE: u32 = 1 << PROB_BITS;
/// A fresh probability: 0 and 1 equally likely.
pub const PROB_INIT: Prob = (PROB_ONE / 2) as Prob;
/// Adaptation speed: each bit moves the probability 1/32 of the way.
const MOVE_BITS: u32 = 5;
/// Once `range` drops below this, shift a byte out to keep precision.
const TOP: u32 = 1 << 24;

/// Writes bits into a range-coded byte stream.
pub struct Encoder {
    /// Bottom of the interval. Bit 32 holds a pending carry.
    low: u64,
    range: u32,
    /// The byte about to be written, held back because a carry could still
    /// ripple into it...
    cache: u8,
    /// ...along with this many minus one 0xFF bytes after it.
    cache_size: u64,
    out: Vec<u8>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            out: Vec::with_capacity(bytes),
        }
    }

    /// Code one bit with an adaptive probability, then update it.
    #[inline]
    pub fn encode_bit(&mut self, prob: &mut Prob, bit: u32) {
        let bound = (self.range >> PROB_BITS) * u32::from(*prob);
        if bit == 0 {
            self.range = bound;
            *prob += ((PROB_ONE - u32::from(*prob)) >> MOVE_BITS) as Prob;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
            *prob -= *prob >> MOVE_BITS;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    /// Code one bit with a probability the caller manages: `p0` is the
    /// chance that `bit` is 0, out of 65536, and must be in `1..=65535`.
    /// This lets models richer than [`Prob`] drive the coder.
    #[inline]
    pub fn encode_with(&mut self, p0: u32, bit: u32) {
        debug_assert!((1..=0xFFFF).contains(&p0));
        let bound = (self.range >> 16) * p0;
        if bit == 0 {
            self.range = bound;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    /// Code the low `bits` bits of `value`, most significant first, each at
    /// probability 1/2. For values that don't compress, like LZMA's high
    /// distance bits.
    pub fn encode_direct(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.range >>= 1;
            if (value >> i) & 1 == 1 {
                self.low += u64::from(self.range);
            }
            while self.range < TOP {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }

    /// Code a `bits`-bit value as a walk down a binary tree, most
    /// significant bit first. `probs` needs `1 << bits` entries (index 0 is
    /// unused); node `n`'s children are `2n` and `2n + 1`.
    #[inline]
    pub fn encode_tree(&mut self, probs: &mut [Prob], bits: u32, value: u32) {
        let mut node = 1;
        for i in (0..bits).rev() {
            let bit = (value >> i) & 1;
            self.encode_bit(&mut probs[node], bit);
            node = (node << 1) | bit as usize;
        }
    }

    /// Like [`encode_tree`](Self::encode_tree) but least significant bit
    /// first. LZMA codes the low bits of distances this way.
    #[inline]
    pub fn encode_reverse_tree(&mut self, probs: &mut [Prob], bits: u32, mut value: u32) {
        let mut node = 1;
        for _ in 0..bits {
            let bit = value & 1;
            value >>= 1;
            self.encode_bit(&mut probs[node], bit);
            node = (node << 1) | bit as usize;
        }
    }

    /// Flush the interval and return the stream.
    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }

    /// Move the top byte of `low` out. It can't be written yet if it is
    /// 0xFF, because a later carry would turn it into 0x00 and add 1 to the
    /// byte before it. So bytes wait in `cache` until a carry is impossible.
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || self.low >> 32 != 0 {
            let carry = (self.low >> 32) as u8;
            let mut byte = self.cache;
            loop {
                self.out.push(byte.wrapping_add(carry));
                byte = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = u64::from((self.low as u32) << 8);
    }
}

/// Reads bits back from a range-coded stream.
///
/// Reading past the end of the input yields zero bytes instead of an error,
/// which keeps the per-bit path branch-free. Callers bound their loops by the
/// expected output size and then call [`Decoder::finish`], which reports the
/// overrun.
pub struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Decoder<'a> {
    pub fn new(input: &'a [u8]) -> crate::Result<Self> {
        // The encoder's first byte is always 0: the empty cache it started with.
        match input.first() {
            None => return Err(crate::Error::UnexpectedEof),
            Some(0) => {}
            Some(_) => {
                return Err(crate::Error::Corrupt(
                    "range coder stream must start with 0",
                ));
            }
        }
        let mut d = Self {
            input,
            pos: 1,
            range: u32::MAX,
            code: 0,
        };
        for _ in 0..4 {
            d.code = (d.code << 8) | u32::from(d.next_byte());
        }
        Ok(d)
    }

    #[inline]
    fn next_byte(&mut self) -> u8 {
        let b = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    #[inline]
    fn normalize(&mut self) {
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(self.next_byte());
        }
    }

    /// Decode one bit coded with [`Encoder::encode_bit`], updating `prob`
    /// the same way the encoder did.
    #[inline]
    pub fn decode_bit(&mut self, prob: &mut Prob) -> u32 {
        let bound = (self.range >> PROB_BITS) * u32::from(*prob);
        let bit = if self.code < bound {
            self.range = bound;
            *prob += ((PROB_ONE - u32::from(*prob)) >> MOVE_BITS) as Prob;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            *prob -= *prob >> MOVE_BITS;
            1
        };
        self.normalize();
        bit
    }

    /// Inverse of [`Encoder::encode_with`].
    #[inline]
    pub fn decode_with(&mut self, p0: u32) -> u32 {
        let bound = (self.range >> 16) * p0;
        let bit = if self.code < bound {
            self.range = bound;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            1
        };
        self.normalize();
        bit
    }

    /// Inverse of [`Encoder::encode_direct`].
    pub fn decode_direct(&mut self, bits: u32) -> u32 {
        let mut value = 0;
        for _ in 0..bits {
            self.range >>= 1;
            let bit = u32::from(self.code >= self.range);
            if bit == 1 {
                self.code -= self.range;
            }
            value = (value << 1) | bit;
            self.normalize();
        }
        value
    }

    /// Inverse of [`Encoder::encode_tree`].
    #[inline]
    pub fn decode_tree(&mut self, probs: &mut [Prob], bits: u32) -> u32 {
        let mut node = 1;
        for _ in 0..bits {
            node = (node << 1) | self.decode_bit(&mut probs[node]) as usize;
        }
        (node - (1 << bits)) as u32
    }

    /// Inverse of [`Encoder::encode_reverse_tree`].
    #[inline]
    pub fn decode_reverse_tree(&mut self, probs: &mut [Prob], bits: u32) -> u32 {
        let mut node = 1;
        let mut value = 0;
        for i in 0..bits {
            let bit = self.decode_bit(&mut probs[node]);
            node = (node << 1) | bit as usize;
            value |= bit << i;
        }
        value
    }

    /// Bytes of input consumed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// True once the decoder has read past the end of the input, which a
    /// valid stream never does. Lets callers that can't bound their output
    /// in advance stop on garbage.
    pub fn overrun(&self) -> bool {
        self.pos > self.input.len()
    }

    /// Check the stream was used exactly: a valid stream is read to its
    /// last byte and not beyond.
    pub fn finish(self) -> crate::Result<()> {
        use std::cmp::Ordering::*;
        match self.pos.cmp(&self.input.len()) {
            Equal => Ok(()),
            Greater => Err(crate::Error::UnexpectedEof),
            Less => Err(crate::Error::Corrupt(
                "data after end of range coder stream",
            )),
        }
    }
}

/// Price (cost in bits) of coding a bit, in units of 1/16 bit.
pub const PRICE_ONE_BIT: u32 = 16;

/// Estimated cost of coding `bit` with probability `prob`, in 1/16 bits:
/// `-log2(P(bit))`. Encoders use it to choose between ways of coding the
/// same data. Precision is kept low (128 buckets) so a table lookup does.
#[inline]
pub fn price_bit(prob: Prob, bit: u32) -> u32 {
    let p = if bit == 0 {
        u32::from(prob)
    } else {
        PROB_ONE - u32::from(prob)
    };
    price_table()[(p >> 4) as usize]
}

/// Price of coding `value` with [`Encoder::encode_tree`].
pub fn price_tree(probs: &[Prob], bits: u32, value: u32) -> u32 {
    let mut node = 1;
    let mut price = 0;
    for i in (0..bits).rev() {
        let bit = (value >> i) & 1;
        price += price_bit(probs[node], bit);
        node = (node << 1) | bit as usize;
    }
    price
}

fn price_table() -> &'static [u32; 128] {
    static TABLE: std::sync::OnceLock<[u32; 128]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            // Middle of the bucket; bucket 0 is clamped to the lowest
            // probability the adaptive update can reach.
            let p = ((i as f64 * 16.0 + 8.0).max(31.0)) / f64::from(PROB_ONE);
            (-p.log2() * f64::from(PRICE_ONE_BIT)).round() as u32
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bits with a skewed distribution, deterministic.
    fn skewed_bits(n: usize, one_in: u64) -> Vec<u32> {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                u32::from(s % one_in == 0)
            })
            .collect()
    }

    #[test]
    fn bits_roundtrip_and_stream_is_used_exactly() {
        for one_in in [1, 2, 3, 50, 1000] {
            let bits = skewed_bits(10_000, one_in);
            let mut enc = Encoder::new();
            let mut p = PROB_INIT;
            for &b in &bits {
                enc.encode_bit(&mut p, b);
            }
            let packed = enc.finish();
            let mut dec = Decoder::new(&packed).unwrap();
            let mut p = PROB_INIT;
            for &b in &bits {
                assert_eq!(dec.decode_bit(&mut p), b);
            }
            dec.finish().unwrap();
        }
    }

    #[test]
    fn predictable_bits_cost_a_fraction_of_a_bit() {
        // 1 in 1000 bits is a 1: entropy is about 0.011 bits per bit. The
        // adaptive estimate can't get that close (the probability floor is
        // 31/2048), but it is far below Huffman's 1 bit.
        let bits = skewed_bits(100_000, 1000);
        let mut enc = Encoder::new();
        let mut p = PROB_INIT;
        for &b in &bits {
            enc.encode_bit(&mut p, b);
        }
        let packed = enc.finish();
        assert!(packed.len() * 8 < bits.len() / 20, "{} bytes", packed.len());
    }

    #[test]
    fn direct_and_tree_roundtrip() {
        let values: Vec<u32> = (0..2000u32)
            .map(|i| i.wrapping_mul(2_654_435_761) >> 12)
            .collect();
        let mut enc = Encoder::new();
        let mut tree = vec![PROB_INIT; 1 << 8];
        for &v in &values {
            enc.encode_direct(v, 20);
            enc.encode_tree(&mut tree, 8, v & 0xFF);
        }
        let packed = enc.finish();
        let mut dec = Decoder::new(&packed).unwrap();
        let mut tree = vec![PROB_INIT; 1 << 8];
        for &v in &values {
            assert_eq!(dec.decode_direct(20), v);
            assert_eq!(dec.decode_tree(&mut tree, 8), v & 0xFF);
        }
        dec.finish().unwrap();
    }

    #[test]
    fn carries_propagate_through_ff_runs() {
        // Always coding the unlikely bit pushes `low` up hard, producing
        // 0xFF bytes and carries into them.
        let mut enc = Encoder::new();
        let mut probs = [PROB_INIT; 4];
        let pattern = |i: usize| u32::from(i % 7 != 0);
        for i in 0..50_000 {
            enc.encode_bit(&mut probs[i % 4], pattern(i));
            enc.encode_direct(u32::MAX, 3);
        }
        let packed = enc.finish();
        let mut dec = Decoder::new(&packed).unwrap();
        let mut probs = [PROB_INIT; 4];
        for i in 0..50_000 {
            assert_eq!(dec.decode_bit(&mut probs[i % 4]), pattern(i), "bit {i}");
            assert_eq!(dec.decode_direct(3), 7);
        }
        dec.finish().unwrap();
    }

    #[test]
    fn reverse_tree_roundtrip_and_prices() {
        let mut enc = Encoder::new();
        let mut probs = [PROB_INIT; 16];
        for v in 0..500u32 {
            enc.encode_reverse_tree(&mut probs, 4, v % 16);
        }
        let packed = enc.finish();
        let mut dec = Decoder::new(&packed).unwrap();
        let mut probs = [PROB_INIT; 16];
        for v in 0..500u32 {
            assert_eq!(dec.decode_reverse_tree(&mut probs, 4), v % 16);
        }
        dec.finish().unwrap();

        assert_eq!(price_bit(PROB_INIT, 0), PRICE_ONE_BIT);
        assert_eq!(price_bit(PROB_INIT, 1), PRICE_ONE_BIT);
        assert!(price_bit(2000, 0) < 2 && price_bit(2000, 1) > 5 * PRICE_ONE_BIT);
        assert_eq!(price_tree(&[PROB_INIT; 256], 8, 77), 8 * PRICE_ONE_BIT);
    }

    #[test]
    fn caller_managed_probabilities_roundtrip() {
        let bits = skewed_bits(20_000, 9);
        let p0 = |i: usize| 1 + (i as u32 * 7919) % 65535;
        let mut enc = Encoder::new();
        for (i, &b) in bits.iter().enumerate() {
            enc.encode_with(p0(i), b);
        }
        let packed = enc.finish();
        let mut dec = Decoder::new(&packed).unwrap();
        for (i, &b) in bits.iter().enumerate() {
            assert_eq!(dec.decode_with(p0(i)), b);
        }
        dec.finish().unwrap();
    }

    #[test]
    fn truncated_and_padded_streams_are_rejected() {
        let mut enc = Encoder::new();
        let mut p = PROB_INIT;
        for i in 0..1000 {
            enc.encode_bit(&mut p, i & 1);
        }
        let packed = enc.finish();

        let short = &packed[..packed.len() - 1];
        let mut dec = Decoder::new(short).unwrap();
        let mut p = PROB_INIT;
        for _ in 0..1000 {
            dec.decode_bit(&mut p);
        }
        assert_eq!(dec.finish(), Err(crate::Error::UnexpectedEof));

        let mut long = packed.clone();
        long.push(0);
        let mut dec = Decoder::new(&long).unwrap();
        let mut p = PROB_INIT;
        for _ in 0..1000 {
            dec.decode_bit(&mut p);
        }
        assert!(dec.finish().is_err());

        assert!(Decoder::new(&[1, 2, 3, 4, 5]).is_err());
        assert!(Decoder::new(&[]).is_err());
    }
}
