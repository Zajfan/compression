//! Bit-level run-length encoding with Elias gamma lengths.
//!
//! Same idea as [`PackBits`](super::PackBits), with one change: lengths are
//! written as variable-length [gamma codes](crate::bits::BitWriter::write_gamma)
//! instead of fixed 7-bit fields, so there is no 128-byte packet limit. A run
//! of a million zeros takes about 6 bytes instead of 16 KB.
//!
//! The stream is a sequence of tokens:
//!
//! ```text
//! literal: 0, gamma(len),     then len raw bytes (8 bits each)
//! run:     1, gamma(len - 2), then the repeated byte
//! ```
//!
//! Runs are at least [`MIN_RUN`] long, so `len - 2 ≥ 1` as gamma requires.
//! The decoder stops once it has produced the expected number of bytes.
//!
//! Trade-off versus PackBits: literals are no longer byte-aligned, so
//! decoding is slower (every byte goes through the bit reader).

use super::packbits::run_length;
use crate::bits::{BitReader, BitWriter};
use crate::{Codec, Error, Result};

/// Shortest run worth its own token. A run token costs 1 flag bit, the gamma
/// length and 8 bits for the byte. Breaking a literal also costs another
/// literal header later, so short runs are cheaper left in the literal.
pub const MIN_RUN: usize = 4;

#[derive(Debug, Clone, Copy, Default)]
pub struct Rle;

impl Codec for Rle {
    fn id(&self) -> u8 {
        2
    }

    fn name(&self) -> &'static str {
        "rle"
    }

    fn description(&self) -> &'static str {
        "Bit-level run-length encoding with Elias gamma lengths"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::with_capacity(input.len() / 2);
        let mut literal_start = 0;
        let mut i = 0;
        while i < input.len() {
            let run = run_length(&input[i..], usize::MAX);
            if run >= MIN_RUN {
                write_literal(&mut w, &input[literal_start..i]);
                w.write_bit(true);
                w.write_gamma((run - 2) as u64);
                w.write_byte(input[i]);
                literal_start = i + run;
            }
            i += run;
        }
        write_literal(&mut w, &input[literal_start..]);
        w.finish()
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(expected_len);
        let mut r = BitReader::new(input);
        while out.len() < expected_len {
            let is_run = r.read_bit()?;
            let len = r.read_gamma()?;
            let len = if is_run {
                len.checked_add(2)
            } else {
                Some(len)
            }
            .and_then(|l| usize::try_from(l).ok())
            .filter(|&l| l <= expected_len - out.len())
            .ok_or(Error::Corrupt("token longer than remaining output"))?;
            if is_run {
                let byte = r.read_byte()?;
                out.resize(out.len() + len, byte);
            } else {
                for _ in 0..len {
                    out.push(r.read_byte()?);
                }
            }
        }
        Ok(out)
    }
}

fn write_literal(w: &mut BitWriter, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    w.write_bit(false);
    w.write_gamma(bytes.len() as u64);
    for &b in bytes {
        w.write_byte(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn million_zeros_is_tiny() {
        let packed = Rle.compress(&vec![0; 1_000_000]);
        // 1 flag + 39 gamma bits + 8 byte bits = 48 bits.
        assert_eq!(packed.len(), 6);
    }

    #[test]
    fn random_data_barely_grows() {
        let data = noise(10_000);
        let packed = Rle.compress(&data);
        assert!(packed.len() <= data.len() + 8, "{} bytes", packed.len());
    }

    #[test]
    fn rejects_bomb() {
        // Run token claiming a huge length when 10 bytes are expected.
        let mut w = BitWriter::new();
        w.write_bit(true);
        w.write_gamma(1_000_000);
        w.write_byte(0);
        assert!(Rle.decompress(&w.finish(), 10).is_err());
    }

    /// Simple xorshift noise with no runs worth encoding.
    fn noise(len: usize) -> Vec<u8> {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }
}
