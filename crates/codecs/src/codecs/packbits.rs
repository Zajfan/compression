//! PackBits: the classic byte-oriented run-length encoding.
//!
//! Used by TIFF images and Apple's MacPaint. The output is a sequence of
//! packets, each starting with one header byte `h`:
//!
//! | header `h`  | meaning                                          |
//! |-------------|--------------------------------------------------|
//! | 0..=127     | literal: copy the next `h + 1` bytes as-is (1–128) |
//! | 129..=255   | run: repeat the next byte `257 - h` times (2–128)  |
//! | 128         | unused (rejected as corrupt)                     |
//!
//! Strength: never grows data by more than 1 byte per 128 (0.8%).
//! Weakness: a run costs 2 bytes per 128 repeats, so 1 MB of zeros still
//! takes 16 KB. The bit-level [`Rle`](super::Rle) codec fixes that.

use crate::{Codec, Error, Result};

/// Longest literal or run one packet can hold.
const MAX_PACKET: usize = 128;
/// Shorter runs are cheaper to leave inside a literal packet: a 2-byte run
/// costs 2 bytes either way, but splitting a literal adds a header.
const MIN_RUN: usize = 3;

#[derive(Debug, Clone, Copy, Default)]
pub struct PackBits;

impl Codec for PackBits {
    fn id(&self) -> u8 {
        1
    }

    fn name(&self) -> &'static str {
        "packbits"
    }

    fn description(&self) -> &'static str {
        "Byte-oriented run-length encoding (TIFF PackBits)"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() + input.len() / MAX_PACKET + 1);
        let mut literal_start = 0;
        let mut i = 0;
        while i < input.len() {
            let run = run_length(&input[i..], MAX_PACKET);
            if run >= MIN_RUN {
                emit_literals(&mut out, &input[literal_start..i]);
                out.push((257 - run) as u8);
                out.push(input[i]);
                i += run;
                literal_start = i;
            } else {
                i += run;
            }
        }
        emit_literals(&mut out, &input[literal_start..]);
        out
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(expected_len);
        let mut i = 0;
        while i < input.len() {
            let header = input[i] as usize;
            i += 1;
            match header {
                0..=127 => {
                    let n = header + 1;
                    let bytes = input.get(i..i + n).ok_or(Error::UnexpectedEof)?;
                    check_room(&out, n, expected_len)?;
                    out.extend_from_slice(bytes);
                    i += n;
                }
                128 => return Err(Error::Corrupt("packbits header 128 is unused")),
                _ => {
                    let n = 257 - header;
                    let &byte = input.get(i).ok_or(Error::UnexpectedEof)?;
                    check_room(&out, n, expected_len)?;
                    out.resize(out.len() + n, byte);
                    i += 1;
                }
            }
        }
        Ok(out)
    }
}

/// Number of times `data[0]` repeats at the start of `data`, up to `max`.
pub(crate) fn run_length(data: &[u8], max: usize) -> usize {
    let first = data[0];
    data.iter().take(max).take_while(|&&b| b == first).count()
}

/// Write `bytes` as literal packets of at most 128 bytes each.
fn emit_literals(out: &mut Vec<u8>, bytes: &[u8]) {
    for chunk in bytes.chunks(MAX_PACKET) {
        out.push((chunk.len() - 1) as u8);
        out.extend_from_slice(chunk);
    }
}

/// Refuse to grow past the original size: corrupt input must not be able to
/// make us allocate unbounded memory (a "decompression bomb").
fn check_room(out: &[u8], n: usize, expected_len: usize) -> Result<()> {
    if out.len() + n > expected_len {
        Err(Error::Corrupt("decoded data longer than recorded size"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_encoding() {
        // The worked example from Apple's Technical Note TN1023.
        let input = [
            0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0x22,
            0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA,
        ];
        let expected = [
            0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A, 0xFD, 0xAA, 0x03, 0x80, 0x00, 0x2A, 0x22, 0xF7,
            0xAA,
        ];
        assert_eq!(PackBits.compress(&input), expected);
        assert_eq!(PackBits.decompress(&expected, input.len()).unwrap(), input);
    }

    #[test]
    fn worst_case_expansion_is_bounded() {
        let data: Vec<u8> = (0..=255).cycle().take(128 * 100).collect();
        assert_eq!(PackBits.compress(&data).len(), data.len() + 100);
    }

    #[test]
    fn long_run() {
        let packed = PackBits.compress(&[7; 1000]);
        // 7 full runs of 128 + one run of 104, 2 bytes each.
        assert_eq!(packed.len(), 16);
    }

    #[test]
    fn rejects_bomb() {
        // A run of 128 when only 10 bytes are expected.
        assert!(PackBits.decompress(&[0x81, 0], 10).is_err());
    }
}
