use crate::bwt::{compress, decompress};
use crate::{Codec, Result};

/// Burrows–Wheeler transform + move-to-front + run-length + rANS, the
/// pipeline bzip2 is built on (not the `.bz2` file format itself; see
/// [`crate::bwt`]), 900K blocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bwt;

impl Codec for Bwt {
    fn id(&self) -> u8 {
        10
    }

    fn name(&self) -> &'static str {
        "bwt"
    }

    fn description(&self) -> &'static str {
        "Burrows-Wheeler transform + move-to-front + run-length + rANS (bzip2-style), 900K blocks"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        compress(input)
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        decompress(input, expected_len)
    }
}
