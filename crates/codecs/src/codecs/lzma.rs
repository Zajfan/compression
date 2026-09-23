use crate::lzma::{Options, compress, decompress};
use crate::{Codec, Error, Result};

/// LZMA in the `.lzma` format, 16 MB dictionary, optimal parse.
#[derive(Debug, Clone, Copy, Default)]
pub struct Lzma;

impl Codec for Lzma {
    fn id(&self) -> u8 {
        9
    }

    fn name(&self) -> &'static str {
        "lzma"
    }

    fn description(&self) -> &'static str {
        "LZMA (LZ77 + range coder + context models), the 7-Zip method, 16 MB window"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        compress(input, Options::default())
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let out = decompress(input, expected_len)?;
        if out.len() != expected_len {
            return Err(Error::LengthMismatch {
                expected: expected_len,
                actual: out.len(),
            });
        }
        Ok(out)
    }
}
