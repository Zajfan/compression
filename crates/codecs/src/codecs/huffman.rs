//! Order-0 Huffman coding of bytes.
//!
//! Counts how often each byte value occurs in the whole input, builds one
//! Huffman code from those counts, and writes every byte with it. Common
//! bytes (like `e` and space in English) get short codes, rare ones long.
//!
//! "Order-0" means each byte is coded on its own, ignoring what came before.
//! That caps how well it can do: the best possible is the order-0 entropy
//! (`cmpr entropy` shows it). Beating that needs context or matches, which
//! later codecs add.
//!
//! Stream layout (bits, LSB-first):
//!
//! ```text
//! 256 × 4 bits   code length of each byte value (0 = does not occur)
//! ...            the Huffman code of every input byte
//! ```
//!
//! The 128-byte table is a fixed cost; Deflate stores it more cleverly,
//! which we'll do when we get there. Empty input produces empty output.

use crate::bits::{BitReader, BitWriter};
use crate::huffman::{Decoder, Encoder, code_lengths};
use crate::{Codec, Error, Result};

/// Code lengths are stored in 4 bits, so 15 is the cap here too.
const MAX_LEN: u32 = 15;

#[derive(Debug, Clone, Copy, Default)]
pub struct Huffman;

impl Codec for Huffman {
    fn id(&self) -> u8 {
        3
    }

    fn name(&self) -> &'static str {
        "huffman"
    }

    fn description(&self) -> &'static str {
        "Order-0 Huffman coding of bytes (canonical, 15-bit max)"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        if input.is_empty() {
            return Vec::new();
        }
        let mut counts = [0u64; 256];
        for &b in input {
            counts[b as usize] += 1;
        }
        let lengths = code_lengths(&counts, MAX_LEN);

        let mut w = BitWriter::with_capacity(128 + input.len() / 2);
        for &len in &lengths {
            w.write_bits(u32::from(len), 4);
        }
        let enc = Encoder::from_lengths(&lengths);
        for &b in input {
            enc.write(&mut w, b as usize);
        }
        w.finish()
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        if expected_len == 0 {
            return Ok(Vec::new());
        }
        let mut r = BitReader::new(input);
        let mut lengths = [0u8; 256];
        for len in &mut lengths {
            *len = r.read_bits(4)? as u8;
        }
        if lengths.iter().all(|&l| l == 0) {
            return Err(Error::Corrupt("huffman table is empty"));
        }
        let dec = Decoder::from_lengths(&lengths)?;
        let mut out = Vec::with_capacity(expected_len);
        for _ in 0..expected_len {
            out.push(dec.read(&mut r)? as u8);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::entropy_bits_per_byte;

    #[test]
    fn close_to_entropy_on_text() {
        let text = b"it was the best of times, it was the worst of times. ".repeat(200);
        let packed = Huffman.compress(&text);
        let ideal_bits = entropy_bits_per_byte(&text) * text.len() as f64;
        let actual_bits = (packed.len() - 128) as f64 * 8.0;
        // Huffman loses at most 1 bit per byte versus entropy; on real text
        // it is usually within a few percent.
        assert!(
            actual_bits < ideal_bits * 1.05,
            "{actual_bits} vs {ideal_bits}"
        );
    }

    #[test]
    fn single_symbol_costs_one_bit_each() {
        let packed = Huffman.compress(&[b'z'; 800]);
        assert_eq!(packed.len(), 128 + 100);
    }
}
