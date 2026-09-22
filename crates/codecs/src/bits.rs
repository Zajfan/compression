//! Reading and writing individual bits.
//!
//! Compressors think in bits, not bytes: a common symbol might get a 2-bit
//! code and a rare one 13 bits. These types pack such codes tightly into
//! bytes and unpack them again.
//!
//! **Bit order:** bits fill each byte starting from the least significant
//! bit (LSB-first). This is the order Deflate (zip, gzip, png) uses, so the
//! same reader and writer will serve our Deflate implementation later.
//!
//! ```
//! use cmpr_codecs::bits::{BitReader, BitWriter};
//!
//! let mut w = BitWriter::new();
//! w.write_bits(0b101, 3); // 3-bit value
//! w.write_bit(true);
//! w.write_gamma(9);       // variable-length number
//! let bytes = w.finish();
//!
//! let mut r = BitReader::new(&bytes);
//! assert_eq!(r.read_bits(3).unwrap(), 0b101);
//! assert!(r.read_bit().unwrap());
//! assert_eq!(r.read_gamma().unwrap(), 9);
//! ```

use crate::{Error, Result};

/// Packs bit fields into a byte buffer, LSB-first.
#[derive(Debug, Default)]
pub struct BitWriter {
    out: Vec<u8>,
    /// Pending bits not yet flushed to `out`, lowest bit first.
    acc: u64,
    /// Number of valid bits in `acc` (always < 8 between calls).
    nbits: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            out: Vec::with_capacity(bytes),
            ..Self::default()
        }
    }

    /// Write the low `n` bits of `value` (`n` ≤ 32), lowest bit first.
    pub fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        debug_assert!(n == 32 || value >> n == 0, "value has more than {n} bits");
        self.acc |= u64::from(value) << self.nbits;
        self.nbits += n;
        while self.nbits >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.nbits -= 8;
        }
    }

    pub fn write_bit(&mut self, bit: bool) {
        self.write_bits(u32::from(bit), 1);
    }

    /// Write a whole byte (8 bits). Does not need to be byte-aligned.
    pub fn write_byte(&mut self, byte: u8) {
        self.write_bits(u32::from(byte), 8);
    }

    /// Write `n` ≥ 1 as an [Elias gamma code](https://en.wikipedia.org/wiki/Elias_gamma_coding).
    ///
    /// A number with `k` significant bits is written as `k - 1` zero bits,
    /// a one bit, then its remaining `k - 1` bits. Small numbers are cheap:
    ///
    /// | n | bits |
    /// |---|------|
    /// | 1 | 1 |
    /// | 2–3 | 3 |
    /// | 4–7 | 5 |
    /// | 1 000 000 | 39 |
    ///
    /// The decoder needs no length prefix, because the zeros say how many
    /// bits follow.
    pub fn write_gamma(&mut self, n: u64) {
        assert!(n >= 1, "Elias gamma cannot encode 0");
        let k = 64 - n.leading_zeros(); // significant bits
        let extra = k - 1;
        // Leading zeros, in chunks because write_bits takes at most 32.
        let mut zeros = extra;
        while zeros > 0 {
            let chunk = zeros.min(32);
            self.write_bits(0, chunk);
            zeros -= chunk;
        }
        self.write_bit(true);
        // The bits below the leading one, low 32 first then the rest.
        let rest = n & !(1u64 << extra);
        if extra > 32 {
            self.write_bits(rest as u32, 32);
            self.write_bits((rest >> 32) as u32, extra - 32);
        } else {
            self.write_bits(rest as u32, extra);
        }
    }

    /// Pad with zero bits up to the next byte boundary.
    pub fn align_to_byte(&mut self) {
        if self.nbits > 0 {
            self.write_bits(0, 8 - self.nbits);
        }
    }

    /// Total bits written so far.
    pub fn bit_len(&self) -> u64 {
        self.out.len() as u64 * 8 + u64::from(self.nbits)
    }

    /// Flush any partial byte (padded with zeros) and return the buffer.
    pub fn finish(mut self) -> Vec<u8> {
        self.align_to_byte();
        self.out
    }
}

/// Reads bit fields written by [`BitWriter`].
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Next byte of `data` to load into `acc`.
    pos: usize,
    acc: u64,
    nbits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            acc: 0,
            nbits: 0,
        }
    }

    /// Read `n` bits (`n` ≤ 32).
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        while self.nbits < n {
            let &byte = self.data.get(self.pos).ok_or(Error::UnexpectedEof)?;
            self.acc |= u64::from(byte) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
        let value = (self.acc & ((1u64 << n) - 1)) as u32;
        self.acc >>= n;
        self.nbits -= n;
        Ok(value)
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits(1)? == 1)
    }

    pub fn read_byte(&mut self) -> Result<u8> {
        Ok(self.read_bits(8)? as u8)
    }

    /// Read a number written by [`BitWriter::write_gamma`].
    pub fn read_gamma(&mut self) -> Result<u64> {
        let mut extra = 0;
        while !self.read_bit()? {
            extra += 1;
            if extra > 63 {
                return Err(Error::Corrupt("gamma code longer than 64 bits"));
            }
        }
        let rest = if extra > 32 {
            let low = u64::from(self.read_bits(32)?);
            low | u64::from(self.read_bits(extra - 32)?) << 32
        } else {
            u64::from(self.read_bits(extra)?)
        };
        Ok(1u64 << extra | rest)
    }

    /// Skip to the next byte boundary, discarding the padding bits.
    pub fn align_to_byte(&mut self) {
        let drop = self.nbits % 8;
        self.acc >>= drop;
        self.nbits -= drop;
    }

    /// Bits not yet read, including padding in the final byte.
    pub fn bits_remaining(&self) -> u64 {
        (self.data.len() - self.pos) as u64 * 8 + u64::from(self.nbits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn lsb_first_layout() {
        let mut w = BitWriter::new();
        w.write_bit(true); // bit 0
        w.write_bits(0b10, 2); // bits 1-2
        w.write_bits(0b11111, 5); // bits 3-7
        w.write_bits(0xABC, 12); // spans two bytes
        assert_eq!(w.bit_len(), 20);
        assert_eq!(w.finish(), [0b1111_1101, 0xBC, 0x0A]);
    }

    #[test]
    fn gamma_sizes() {
        for (n, bits) in [
            (1, 1),
            (2, 3),
            (3, 3),
            (4, 5),
            (7, 5),
            (8, 7),
            (1_000_000, 39),
        ] {
            let mut w = BitWriter::new();
            w.write_gamma(n);
            assert_eq!(w.bit_len(), bits, "gamma({n})");
        }
    }

    #[test]
    fn gamma_extremes() {
        let values = [1, u64::from(u32::MAX), 1 << 32, (1 << 33) + 5, u64::MAX];
        let mut w = BitWriter::new();
        for v in values {
            w.write_gamma(v);
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for v in values {
            assert_eq!(r.read_gamma().unwrap(), v);
        }
    }

    #[test]
    fn eof_is_an_error() {
        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(r.read_bits(8), Ok(0xFF));
        assert_eq!(r.read_bit(), Err(Error::UnexpectedEof));
        // All-zero input never terminates a gamma code.
        assert!(BitReader::new(&[0; 16]).read_gamma().is_err());
    }

    #[test]
    fn align() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.align_to_byte();
        w.write_byte(0x42);
        let bytes = w.finish();
        assert_eq!(bytes, [0b101, 0x42]);
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3), Ok(0b101));
        r.align_to_byte();
        assert_eq!(r.read_byte(), Ok(0x42));
        assert_eq!(r.bits_remaining(), 0);
    }

    #[derive(Debug, Clone)]
    enum Field {
        Bits(u32, u32),
        Gamma(u64),
    }

    fn field() -> impl Strategy<Value = Field> {
        prop_oneof![
            (0u32..=32).prop_flat_map(|n| {
                let max = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
                (0..=max).prop_map(move |v| Field::Bits(v, n))
            }),
            (1u64..=u64::MAX).prop_map(Field::Gamma),
            (1u64..1000).prop_map(Field::Gamma),
        ]
    }

    proptest! {
        #[test]
        fn any_sequence_roundtrips(fields in proptest::collection::vec(field(), 0..200)) {
            let mut w = BitWriter::new();
            for f in &fields {
                match *f {
                    Field::Bits(v, n) => w.write_bits(v, n),
                    Field::Gamma(v) => w.write_gamma(v),
                }
            }
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            for f in &fields {
                match *f {
                    Field::Bits(v, n) => prop_assert_eq!(r.read_bits(n)?, v),
                    Field::Gamma(v) => prop_assert_eq!(r.read_gamma()?, v),
                }
            }
            prop_assert!(r.bits_remaining() < 8);
        }
    }
}
