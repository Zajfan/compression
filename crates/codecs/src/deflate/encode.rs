//! Deflate encoder.
//!
//! 1. LZ77 turns the input into tokens ([`crate::lz77::parse`]).
//! 2. Tokens are cut into blocks. Each block gets its own Huffman codes,
//!    so the codes can adapt as the data changes.
//! 3. For each block we compute the exact size as dynamic, fixed and stored,
//!    and write whichever is smallest.

use super::tables::*;
use crate::bits::BitWriter;
use crate::huffman::{Encoder, code_lengths};
use crate::lz77::{self, Params, Token};

/// Tokens per block. zlib uses 16K by default.
const BLOCK_TOKENS: usize = 16 * 1024;
/// Stored blocks hold at most this many bytes (16-bit length field).
const MAX_STORED: usize = 65_535;

/// Compress `input` as a raw Deflate stream. `level` is 0 (store only) to 9.
pub fn deflate(input: &[u8], level: u8) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(input.len() / 2 + 64);
    if input.is_empty() {
        // One final fixed block holding just end-of-block.
        w.write_bit(true);
        w.write_bits(1, 2);
        Encoder::from_lengths(&fixed_litlen_lengths()).write(&mut w, END_OF_BLOCK);
        return w.finish();
    }
    if level == 0 {
        write_stored(&mut w, input, true);
        return w.finish();
    }

    let tokens = lz77::parse(input, Params::level(level));
    let blocks = tokens.len().div_ceil(BLOCK_TOKENS);
    let mut pos = 0;
    for (i, block) in tokens.chunks(BLOCK_TOKENS).enumerate() {
        let len: usize = block.iter().map(|t| t.byte_len()).sum();
        write_block(&mut w, block, &input[pos..pos + len], i + 1 == blocks);
        pos += len;
    }
    w.finish()
}

/// Symbol counts for one block.
struct Counts {
    lit: [u64; NUM_LITLEN],
    dist: [u64; NUM_DIST],
}

impl Counts {
    fn of(tokens: &[Token]) -> Self {
        let mut c = Counts {
            lit: [0; NUM_LITLEN],
            dist: [0; NUM_DIST],
        };
        for &t in tokens {
            match t {
                Token::Literal(b) => c.lit[usize::from(b)] += 1,
                Token::Match { len, dist } => {
                    c.lit[257 + length_code(usize::from(len)).index] += 1;
                    c.dist[dist_code(usize::from(dist)).index] += 1;
                }
            }
        }
        c.lit[END_OF_BLOCK] = 1;
        c
    }

    /// Bits to encode the block's symbols (not the header) with these codes.
    fn cost(&self, lit_lengths: &[u8], dist_lengths: &[u8]) -> u64 {
        let lit: u64 = self
            .lit
            .iter()
            .zip(lit_lengths)
            .map(|(&c, &l)| c * u64::from(l))
            .sum();
        let len_extra: u64 = (0..LENGTH_EXTRA.len())
            .map(|i| self.lit[257 + i] * u64::from(LENGTH_EXTRA[i]))
            .sum();
        let dist: u64 = (0..NUM_DIST)
            .map(|i| self.dist[i] * (u64::from(dist_lengths[i]) + u64::from(DIST_EXTRA[i])))
            .sum();
        lit + len_extra + dist
    }
}

/// A dynamic block's header, ready to write.
struct DynamicHeader {
    lit_lengths: Vec<u8>,
    dist_lengths: Vec<u8>,
    hlit: usize,
    hdist: usize,
    /// Run-length coded code lengths: (symbol 0..=18, extra bits value).
    cl_symbols: Vec<(u8, u32)>,
    cl_lengths: Vec<u8>,
    hclen: usize,
}

impl DynamicHeader {
    fn new(counts: &Counts) -> Self {
        let lit_lengths = code_lengths(&counts.lit, 15);
        let mut dist_lengths = code_lengths(&counts.dist, 15);
        if dist_lengths.iter().all(|&l| l == 0) {
            // No matches in this block. Still send one distance code, as
            // zlib does; some decoders reject an empty distance code.
            dist_lengths[0] = 1;
        }
        let hlit = last_nonzero(&lit_lengths).max(257);
        let hdist = last_nonzero(&dist_lengths).max(1);

        let all: Vec<u8> = lit_lengths[..hlit]
            .iter()
            .chain(&dist_lengths[..hdist])
            .copied()
            .collect();
        let cl_symbols = rle_code_lengths(&all);
        let mut cl_counts = [0u64; 19];
        for &(sym, _) in &cl_symbols {
            cl_counts[usize::from(sym)] += 1;
        }
        let mut cl_lengths = code_lengths(&cl_counts, 7);
        if cl_lengths.iter().filter(|&&l| l > 0).count() == 1 {
            // A one-symbol code is incomplete, which zlib rejects for this
            // code. Add a second, unused symbol to complete it.
            let spare = if cl_lengths[0] == 0 { 0 } else { 1 };
            cl_lengths[spare] = 1;
        }
        let hclen = CL_ORDER
            .iter()
            .rposition(|&i| cl_lengths[i] > 0)
            .map_or(0, |p| p + 1)
            .max(4);
        Self {
            lit_lengths,
            dist_lengths,
            hlit,
            hdist,
            cl_symbols,
            cl_lengths,
            hclen,
        }
    }

    fn bits(&self) -> u64 {
        let symbols: u64 = self
            .cl_symbols
            .iter()
            .map(|&(s, _)| {
                u64::from(self.cl_lengths[usize::from(s)]) + u64::from(extra_bits_for_cl(s))
            })
            .sum();
        5 + 5 + 4 + 3 * self.hclen as u64 + symbols
    }

    fn write(&self, w: &mut BitWriter) {
        w.write_bits((self.hlit - 257) as u32, 5);
        w.write_bits((self.hdist - 1) as u32, 5);
        w.write_bits((self.hclen - 4) as u32, 4);
        for &i in &CL_ORDER[..self.hclen] {
            w.write_bits(u32::from(self.cl_lengths[i]), 3);
        }
        let cl = Encoder::from_lengths(&self.cl_lengths);
        for &(sym, extra) in &self.cl_symbols {
            cl.write(w, usize::from(sym));
            w.write_bits(extra, extra_bits_for_cl(sym));
        }
    }
}

fn last_nonzero(lengths: &[u8]) -> usize {
    lengths.iter().rposition(|&l| l > 0).map_or(0, |p| p + 1)
}

fn extra_bits_for_cl(sym: u8) -> u32 {
    match sym {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

/// Run-length code a sequence of code lengths with Deflate's symbols:
/// 0–15 literal length, 16 = repeat previous 3–6×, 17 = 3–10 zeros,
/// 18 = 11–138 zeros.
fn rle_code_lengths(lengths: &[u8]) -> Vec<(u8, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let value = lengths[i];
        let run = lengths[i..].iter().take_while(|&&l| l == value).count();
        let mut left = run;
        if value == 0 {
            while left >= 11 {
                let n = left.min(138);
                out.push((18, (n - 11) as u32));
                left -= n;
            }
            if left >= 3 {
                out.push((17, (left - 3) as u32));
                left = 0;
            }
        } else {
            out.push((value, 0));
            left -= 1;
            while left >= 3 {
                let n = left.min(6);
                out.push((16, (n - 3) as u32));
                left -= n;
            }
        }
        out.extend(std::iter::repeat_n((value, 0), left));
        i += run;
    }
    out
}

fn write_block(w: &mut BitWriter, tokens: &[Token], raw: &[u8], last: bool) {
    let counts = Counts::of(tokens);
    let header = DynamicHeader::new(&counts);
    let fixed_lit = fixed_litlen_lengths();
    let fixed_dist = fixed_dist_lengths();

    let dynamic_bits = header.bits() + counts.cost(&header.lit_lengths, &header.dist_lengths);
    let fixed_bits = counts.cost(&fixed_lit, &fixed_dist);
    // Per stored block: up to 7 padding bits plus 32 bits of lengths.
    let stored_bits = raw.len().div_ceil(MAX_STORED) as u64 * 39 + raw.len() as u64 * 8;

    if stored_bits < dynamic_bits.min(fixed_bits) {
        write_stored(w, raw, last);
    } else if fixed_bits <= dynamic_bits {
        w.write_bit(last);
        w.write_bits(1, 2);
        write_tokens(w, tokens, &fixed_lit, &fixed_dist);
    } else {
        w.write_bit(last);
        w.write_bits(2, 2);
        header.write(w);
        write_tokens(w, tokens, &header.lit_lengths, &header.dist_lengths);
    }
}

fn write_tokens(w: &mut BitWriter, tokens: &[Token], lit_lengths: &[u8], dist_lengths: &[u8]) {
    let lit = Encoder::from_lengths(lit_lengths);
    let dist = Encoder::from_lengths(dist_lengths);
    for &t in tokens {
        match t {
            Token::Literal(b) => lit.write(w, usize::from(b)),
            Token::Match { len, dist: d } => {
                let l = length_code(usize::from(len));
                lit.write(w, 257 + l.index);
                w.write_bits(l.extra, l.extra_bits);
                let d = dist_code(usize::from(d));
                dist.write(w, d.index);
                w.write_bits(d.extra, d.extra_bits);
            }
        }
    }
    lit.write(w, END_OF_BLOCK);
}

fn write_stored(w: &mut BitWriter, raw: &[u8], last: bool) {
    let chunks = raw.len().div_ceil(MAX_STORED).max(1);
    for (i, chunk) in raw
        .chunks(MAX_STORED)
        .chain((raw.is_empty()).then_some(&[][..]))
        .enumerate()
    {
        w.write_bit(last && i + 1 == chunks);
        w.write_bits(0, 2);
        w.align_to_byte();
        let len = chunk.len() as u32;
        w.write_bits(len, 16);
        w.write_bits(!len & 0xFFFF, 16);
        for &b in chunk {
            w.write_byte(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deflate::inflate;

    #[test]
    fn rle_of_code_lengths() {
        let lengths = [
            8, 8, 8, 8, 8, 8, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 5,
        ];
        assert_eq!(
            rle_code_lengths(&lengths),
            [(8, 0), (16, 3), (18, 1), (5, 0), (5, 0)]
        );
    }

    #[test]
    fn empty_input_matches_zlib() {
        assert_eq!(deflate(b"", 6), [0x03, 0x00]);
    }

    #[test]
    fn roundtrip_every_level() {
        let data = b"Deflate: LZ77 plus Huffman. Deflate: LZ77 plus Huffman! ".repeat(50);
        for level in 0..=9 {
            let packed = deflate(&data, level);
            assert_eq!(
                inflate(&packed, data.len()).unwrap().data,
                data,
                "level {level}"
            );
        }
    }
}
