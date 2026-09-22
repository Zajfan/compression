//! Order-0 rANS with a fresh frequency table per block.
//!
//! The same job as [`Huffman`](super::Huffman) (code each byte by how
//! common it is) but with fractional bits, like the range coders, while
//! decoding many times faster than them. See [`crate::ans`].
//!
//! Frequencies are static, so they have to be sent. Cutting the input into
//! blocks with their own tables lets the model follow data that changes,
//! which is most of what the adaptive [`Range0`](super::Range0) gains over
//! Huffman.
//!
//! Stream layout, repeated for each block of up to `BLOCK` input bytes:
//!
//! ```text
//! 256 × gamma(freq + 1)   frequency of each byte value, out of 4096 (bits, LSB-first)
//! gamma(len + 1)          byte length of the rANS data
//! (pad to a byte)
//! len bytes               rANS data
//! ```
//!
//! Empty input gives empty output.

use crate::ans::{self, Freqs};
use crate::bits::{BitReader, BitWriter};
use crate::{Codec, Error, Result};

/// Input bytes per block.
const BLOCK: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub struct Rans;

impl Codec for Rans {
    fn id(&self) -> u8 {
        8
    }

    fn name(&self) -> &'static str {
        "rans"
    }

    fn description(&self) -> &'static str {
        "Order-0 rANS (asymmetric numeral systems), 4 interleaved states, 32K blocks"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() / 2);
        for block in input.chunks(BLOCK) {
            let mut counts = [0u64; 256];
            for &b in block {
                counts[usize::from(b)] += 1;
            }
            let freqs = Freqs::normalize(&counts).expect("block is not empty");
            let data = ans::encode(block, &freqs);

            let mut w = BitWriter::new();
            for &f in freqs.freqs() {
                w.write_gamma(u64::from(f) + 1);
            }
            w.write_gamma(data.len() as u64 + 1);
            out.extend_from_slice(&w.finish());
            out.extend_from_slice(&data);
        }
        out
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(expected_len);
        let mut pos = 0;
        while out.len() < expected_len {
            let n = (expected_len - out.len()).min(BLOCK);
            let mut r = BitReader::new(&input[pos..]);
            let mut freq = [0u32; 256];
            for f in &mut freq {
                *f = u32::try_from(r.read_gamma()? - 1)
                    .map_err(|_| Error::Corrupt("rANS frequency too large"))?;
            }
            let len = usize::try_from(r.read_gamma()? - 1)
                .map_err(|_| Error::Corrupt("rANS block too large"))?;
            r.align_to_byte();
            pos += r.byte_position();
            let data = input
                .get(pos..)
                .and_then(|rest| rest.get(..len))
                .ok_or(Error::UnexpectedEof)?;
            ans::decode(data, &Freqs::from_freqs(freq)?, n, &mut out)?;
            pos += len;
        }
        if pos != input.len() {
            return Err(Error::Corrupt("data after the last rANS block"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::Huffman;

    #[test]
    fn beats_huffman_on_skewed_data() {
        // 90% one byte. Huffman can't spend less than 1 bit on it, rANS
        // spends about 0.15: 0.75 bits per byte in all, against Huffman's
        // 1.3.
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| {
                if i % 10 == 0 {
                    (i / 10 % 7) as u8
                } else {
                    b'.'
                }
            })
            .collect();
        let rans = Rans.compress(&data).len();
        let huffman = Huffman.compress(&data).len();
        assert!(rans * 3 < huffman * 2, "{rans} vs huffman {huffman}");
    }

    #[test]
    fn multi_block_roundtrip() {
        let mut data = vec![b'a'; BLOCK];
        data.extend((0..=255u8).cycle().take(BLOCK + 17));
        let packed = Rans.compress(&data);
        assert_eq!(Rans.decompress(&packed, data.len()).unwrap(), data);
        assert!(Rans.decompress(&packed, data.len() - 1).is_err());
    }
}
