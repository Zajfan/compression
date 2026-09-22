//! LZSS: LZ77 tokens written with fixed-size fields, no entropy coding.
//!
//! This shows what LZ77 achieves on its own, before Huffman is added on top
//! (which is what [`Deflate`](super::Deflate) does).
//!
//! ```text
//! literal: 0, byte (8 bits)                       = 9 bits
//! match:   1, len - 3 (8 bits), dist - 1 (15 bits) = 24 bits
//! ```
//!
//! A match therefore pays off from 3 bytes (27 bits as literals). The
//! decoder stops after producing the expected number of bytes.

use crate::bits::{BitReader, BitWriter};
use crate::lz77::{self, MIN_MATCH, Params, Token, copy_match};
use crate::{Codec, Error, Result};

#[derive(Debug, Clone, Copy, Default)]
pub struct Lzss;

impl Codec for Lzss {
    fn id(&self) -> u8 {
        4
    }

    fn name(&self) -> &'static str {
        "lzss"
    }

    fn description(&self) -> &'static str {
        "LZ77 matches with fixed-size fields, no Huffman (32 KB window)"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::with_capacity(input.len());
        for t in lz77::parse(input, Params::default()) {
            match t {
                Token::Literal(b) => {
                    w.write_bit(false);
                    w.write_byte(b);
                }
                Token::Match { len, dist } => {
                    w.write_bit(true);
                    w.write_bits(u32::from(len) - MIN_MATCH as u32, 8);
                    w.write_bits(u32::from(dist) - 1, 15);
                }
            }
        }
        w.finish()
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let mut r = BitReader::new(input);
        let mut out = Vec::with_capacity(expected_len);
        while out.len() < expected_len {
            if !r.read_bit()? {
                out.push(r.read_byte()?);
                continue;
            }
            let len = r.read_bits(8)? as usize + MIN_MATCH;
            let dist = r.read_bits(15)? as usize + 1;
            if dist > out.len() {
                return Err(Error::Corrupt("match distance before start of data"));
            }
            if len > expected_len - out.len() {
                return Err(Error::Corrupt("match runs past the end"));
            }
            copy_match(&mut out, dist, len);
        }
        Ok(out)
    }
}
