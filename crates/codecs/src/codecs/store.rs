use crate::{Codec, Error, Result};

/// No compression: output equals input.
///
/// Useful as a baseline, for already-compressed data, and as the simplest
/// possible example of a codec.
#[derive(Debug, Clone, Copy, Default)]
pub struct Store;

impl Codec for Store {
    fn id(&self) -> u8 {
        0
    }

    fn name(&self) -> &'static str {
        "store"
    }

    fn description(&self) -> &'static str {
        "No compression; copies data as-is (baseline)"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        input.to_vec()
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        if input.len() != expected_len {
            return Err(Error::LengthMismatch {
                expected: expected_len,
                actual: input.len(),
            });
        }
        Ok(input.to_vec())
    }
}
