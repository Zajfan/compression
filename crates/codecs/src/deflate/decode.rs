//! Inflate: decode a Deflate stream.

use super::tables::*;
use crate::bits::BitReader;
use crate::huffman::Decoder;
use crate::lz77::copy_match;
use crate::{Error, Result};

/// Result of [`inflate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inflated {
    pub data: Vec<u8>,
    /// Bytes of input the stream occupied. Containers like gzip keep more
    /// data (a checksum) right after it.
    pub consumed: usize,
}

/// Decode one Deflate stream from the start of `input`.
///
/// Fails if the output would exceed `limit` bytes, so a tiny malicious
/// stream can't expand into gigabytes.
pub fn inflate(input: &[u8], limit: usize) -> Result<Inflated> {
    let mut r = BitReader::new(input);
    let mut out = Vec::new();
    let mut fixed: Option<(Decoder, Decoder)> = None;
    loop {
        let last = r.read_bit()?;
        match r.read_bits(2)? {
            0 => stored_block(&mut r, &mut out, limit)?,
            1 => {
                let (lit, dist) = match &fixed {
                    Some(codes) => codes,
                    None => fixed.insert((
                        Decoder::from_lengths(&fixed_litlen_lengths())?,
                        Decoder::from_lengths(&fixed_dist_lengths())?,
                    )),
                };
                huffman_block(&mut r, &mut out, limit, lit, dist)?;
            }
            2 => {
                let (lit, dist) = read_dynamic_header(&mut r)?;
                huffman_block(&mut r, &mut out, limit, &lit, &dist)?;
            }
            _ => return Err(Error::Corrupt("reserved deflate block type 3")),
        }
        if last {
            break;
        }
    }
    r.align_to_byte();
    Ok(Inflated {
        data: out,
        consumed: r.byte_position(),
    })
}

fn check_room(out: &[u8], n: usize, limit: usize) -> Result<()> {
    if out.len() + n > limit {
        Err(Error::Corrupt("deflate output larger than allowed"))
    } else {
        Ok(())
    }
}

fn stored_block(r: &mut BitReader, out: &mut Vec<u8>, limit: usize) -> Result<()> {
    r.align_to_byte();
    let len = r.read_bits(16)?;
    let nlen = r.read_bits(16)?;
    if len != !nlen & 0xFFFF {
        return Err(Error::Corrupt("stored block length check failed"));
    }
    check_room(out, len as usize, limit)?;
    for _ in 0..len {
        out.push(r.read_byte()?);
    }
    Ok(())
}

fn huffman_block(
    r: &mut BitReader,
    out: &mut Vec<u8>,
    limit: usize,
    lit: &Decoder,
    dist: &Decoder,
) -> Result<()> {
    loop {
        let sym = lit.read(r)?;
        if sym < END_OF_BLOCK {
            check_room(out, 1, limit)?;
            out.push(sym as u8);
            continue;
        }
        if sym == END_OF_BLOCK {
            return Ok(());
        }
        let i = sym - 257;
        if i >= LENGTH_BASE.len() {
            return Err(Error::Corrupt("invalid length symbol"));
        }
        let len = usize::from(LENGTH_BASE[i]) + r.read_bits(u32::from(LENGTH_EXTRA[i]))? as usize;
        let d = dist.read(r)?;
        if d >= NUM_DIST {
            return Err(Error::Corrupt("invalid distance symbol"));
        }
        let distance = usize::from(DIST_BASE[d]) + r.read_bits(u32::from(DIST_EXTRA[d]))? as usize;
        if distance > out.len() {
            return Err(Error::Corrupt("match distance before start of data"));
        }
        check_room(out, len, limit)?;
        copy_match(out, distance, len);
    }
}

/// Read a dynamic block's code tables (RFC 1951 §3.2.7).
///
/// The literal and distance code lengths are themselves compressed: they
/// are run-length coded (symbols 16/17/18 mean "repeat") and Huffman coded
/// with a third, small code whose lengths come first.
fn read_dynamic_header(r: &mut BitReader) -> Result<(Decoder, Decoder)> {
    let hlit = r.read_bits(5)? as usize + 257;
    let hdist = r.read_bits(5)? as usize + 1;
    let hclen = r.read_bits(4)? as usize + 4;
    if hlit > NUM_LITLEN || hdist > NUM_DIST {
        return Err(Error::Corrupt("too many deflate codes"));
    }
    let mut cl_lengths = [0u8; 19];
    for &i in &CL_ORDER[..hclen] {
        cl_lengths[i] = r.read_bits(3)? as u8;
    }
    let cl = Decoder::from_lengths(&cl_lengths)?;

    let total = hlit + hdist;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        let (value, repeat) = match cl.read(r)? {
            len @ 0..=15 => (len as u8, 1),
            16 => {
                let &prev = lengths
                    .last()
                    .ok_or(Error::Corrupt("repeat with no previous length"))?;
                (prev, 3 + r.read_bits(2)? as usize)
            }
            17 => (0, 3 + r.read_bits(3)? as usize),
            _ => (0, 11 + r.read_bits(7)? as usize),
        };
        if lengths.len() + repeat > total {
            return Err(Error::Corrupt("code lengths overflow"));
        }
        lengths.resize(lengths.len() + repeat, value);
    }
    let (lit, dist) = lengths.split_at(hlit);
    if lit[END_OF_BLOCK] == 0 {
        return Err(Error::Corrupt("no end-of-block code"));
    }
    Ok((Decoder::from_lengths(lit)?, Decoder::from_lengths(dist)?))
}
