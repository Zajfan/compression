use crate::deflate::{DEFAULT_LEVEL, deflate, inflate};
use crate::{Codec, Error, Result};

/// Raw Deflate stream (RFC 1951) at level 6, byte-compatible with zlib.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deflate;

impl Codec for Deflate {
    fn id(&self) -> u8 {
        5
    }

    fn name(&self) -> &'static str {
        "deflate"
    }

    fn description(&self) -> &'static str {
        "Deflate (LZ77 + Huffman), the format of zip/gzip/png, level 6"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        deflate(input, DEFAULT_LEVEL)
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let inflated = inflate(input, expected_len)?;
        if inflated.consumed != input.len() {
            return Err(Error::Corrupt("data after end of deflate stream"));
        }
        Ok(inflated.data)
    }
}
