//! Minimal single-file frame: header + one codec's output.
//!
//! This is a stepping stone until the real archive format (Phase 3). Layout,
//! all integers little-endian:
//!
//! | offset | size | field                          |
//! |--------|------|--------------------------------|
//! | 0      | 4    | magic `b"CMPR"`                |
//! | 4      | 1    | frame version (currently 0)    |
//! | 5      | 1    | codec id                       |
//! | 6      | 8    | original length in bytes       |
//! | 14     | 4    | CRC-32 of the original data    |
//! | 18     | ..   | codec payload                  |

use crate::{Codec, Error, Result, codec_by_id, crc32::crc32};

pub const MAGIC: [u8; 4] = *b"CMPR";
pub const VERSION: u8 = 0;
pub const HEADER_LEN: usize = 18;

/// Compress `data` with `codec` and wrap it in a frame.
pub fn encode(codec: &dyn Codec, data: &[u8]) -> Vec<u8> {
    let payload = codec.compress(data);
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(codec.id());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Parsed frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub codec_id: u8,
    pub original_len: u64,
    pub crc32: u32,
}

/// Read and validate a frame header.
pub fn read_header(frame: &[u8]) -> Result<Header> {
    if frame.len() < HEADER_LEN {
        return Err(if frame.len() >= 4 && frame[..4] != MAGIC {
            Error::BadMagic
        } else {
            Error::UnexpectedEof
        });
    }
    if frame[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    if frame[4] != VERSION {
        return Err(Error::UnsupportedVersion(frame[4]));
    }
    Ok(Header {
        codec_id: frame[5],
        original_len: u64::from_le_bytes(frame[6..14].try_into().unwrap()),
        crc32: u32::from_le_bytes(frame[14..18].try_into().unwrap()),
    })
}

/// Decode a frame, verifying length and checksum.
pub fn decode(frame: &[u8]) -> Result<Vec<u8>> {
    let header = read_header(frame)?;
    let codec = codec_by_id(header.codec_id).ok_or(Error::UnknownCodec(header.codec_id))?;
    let expected = usize::try_from(header.original_len)
        .map_err(|_| Error::Corrupt("original length does not fit in memory"))?;
    let data = codec.decompress(&frame[HEADER_LEN..], expected)?;
    if data.len() != expected {
        return Err(Error::LengthMismatch {
            expected,
            actual: data.len(),
        });
    }
    let actual = crc32(&data);
    if actual != header.crc32 {
        return Err(Error::ChecksumMismatch {
            expected: header.crc32,
            actual,
        });
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::Store;

    #[test]
    fn roundtrip() {
        let frame = encode(&Store, b"framed");
        assert_eq!(decode(&frame).unwrap(), b"framed");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut frame = encode(&Store, b"x");
        frame[0] = b'Z';
        assert_eq!(decode(&frame), Err(Error::BadMagic));
    }

    #[test]
    fn rejects_truncated() {
        assert_eq!(decode(b"CMPR"), Err(Error::UnexpectedEof));
    }

    #[test]
    fn detects_corruption() {
        let mut frame = encode(&Store, b"some data to damage");
        let last = frame.len() - 1;
        frame[last] ^= 0x01;
        assert!(matches!(
            decode(&frame),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_unknown_codec() {
        let mut frame = encode(&Store, b"x");
        frame[5] = 255;
        assert_eq!(decode(&frame), Err(Error::UnknownCodec(255)));
    }
}
