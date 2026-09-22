//! The gzip file format (RFC 1952): a small header, a Deflate stream, then
//! a CRC-32 and the original size.
//!
//! ```text
//! 1F 8B  08  FLG  MTIME(4)  XFL  OS  [optional fields]  deflate…  CRC32(4)  ISIZE(4)
//! ```
//!
//! A `.gz` file may hold several of these "members" back to back; they
//! decompress to their outputs joined together.
//!
//! Spec: <https://www.rfc-editor.org/rfc/rfc1952>

use crate::crc32::crc32;
use crate::deflate::{deflate, inflate};
use crate::{Error, Result};

const MAGIC: [u8; 2] = [0x1F, 0x8B];
const METHOD_DEFLATE: u8 = 8;
const FHCRC: u8 = 1 << 1;
const FEXTRA: u8 = 1 << 2;
const FNAME: u8 = 1 << 3;
const FCOMMENT: u8 = 1 << 4;
const RESERVED: u8 = 0b1110_0000;
/// "Unknown" operating system. Keeps output identical on every platform.
const OS_UNKNOWN: u8 = 255;

/// Compress `data` into a single-member gzip file.
pub fn compress(data: &[u8], level: u8) -> Vec<u8> {
    let xfl = match level {
        9 => 2,     // "maximum compression"
        0 | 1 => 4, // "fastest"
        _ => 0,
    };
    let mut out = vec![
        MAGIC[0],
        MAGIC[1],
        METHOD_DEFLATE,
        0,
        0,
        0,
        0,
        0,
        xfl,
        OS_UNKNOWN,
    ];
    out.extend(deflate(data, level));
    out.extend(crc32(data).to_le_bytes());
    out.extend((data.len() as u32).to_le_bytes()); // size mod 2^32, per spec
    out
}

/// Decompress a gzip file (all members). Output may not exceed `limit`.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        pos += header_len(&data[pos..])?;
        let inflated = inflate(&data[pos..], limit - out.len())?;
        pos += inflated.consumed;
        let trailer = data.get(pos..pos + 8).ok_or(Error::UnexpectedEof)?;
        let expected_crc = u32::from_le_bytes(trailer[..4].try_into().unwrap());
        let expected_size = u32::from_le_bytes(trailer[4..].try_into().unwrap());
        let actual_crc = crc32(&inflated.data);
        if actual_crc != expected_crc {
            return Err(Error::ChecksumMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }
        if inflated.data.len() as u32 != expected_size {
            return Err(Error::Corrupt("gzip size field does not match"));
        }
        out.extend(inflated.data);
        pos += 8;
        if pos == data.len() {
            return Ok(out);
        }
    }
}

/// Validate a member header and return its length.
fn header_len(data: &[u8]) -> Result<usize> {
    let fixed = data.get(..10).ok_or(Error::UnexpectedEof)?;
    if fixed[..2] != MAGIC {
        return Err(Error::Corrupt("not a gzip file"));
    }
    if fixed[2] != METHOD_DEFLATE {
        return Err(Error::Corrupt("gzip compression method is not deflate"));
    }
    let flags = fixed[3];
    if flags & RESERVED != 0 {
        return Err(Error::Corrupt("reserved gzip flags set"));
    }
    let mut pos = 10;
    if flags & FEXTRA != 0 {
        let xlen = data.get(pos..pos + 2).ok_or(Error::UnexpectedEof)?;
        pos += 2 + usize::from(u16::from_le_bytes([xlen[0], xlen[1]]));
    }
    for flag in [FNAME, FCOMMENT] {
        if flags & flag != 0 {
            let rest = data.get(pos..).ok_or(Error::UnexpectedEof)?;
            pos += rest
                .iter()
                .position(|&b| b == 0)
                .ok_or(Error::UnexpectedEof)?
                + 1;
        }
    }
    if flags & FHCRC != 0 {
        pos += 2;
    }
    if pos > data.len() {
        return Err(Error::UnexpectedEof);
    }
    Ok(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let data = b"hello gzip, hello gzip, hello gzip".repeat(10);
        assert_eq!(decompress(&compress(&data, 6), usize::MAX).unwrap(), data);
    }

    #[test]
    fn multiple_members_concatenate() {
        let mut file = compress(b"first ", 6);
        file.extend(compress(b"second", 1));
        assert_eq!(decompress(&file, usize::MAX).unwrap(), b"first second");
    }

    #[test]
    fn detects_corruption() {
        let mut file = compress(b"some text to protect", 6);
        let n = file.len();
        file[n - 5] ^= 1; // inside the CRC
        assert!(decompress(&file, usize::MAX).is_err());
    }
}
