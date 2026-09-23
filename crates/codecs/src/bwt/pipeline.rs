//! The classic bzip2 pipeline: [`forward`](super::forward) exposes local
//! repetition as runs of the same byte, [`mtf`](super::mtf) turns "which
//! byte" into small numbers biased heavily towards 0, and the runs of 0
//! that fall out of that get coded directly rather than one at a time.
//!
//! Real bzip2 pairs this with a Huffman stage, which is stuck at 1 bit per
//! symbol no matter how likely that symbol is. Since move-to-front's
//! output is dominated by 0s, bzip2 first collapses runs of them into two
//! extra alphabet symbols (`RUNA`/`RUNB`, a bijective base-2 count) so
//! Huffman only pays for "a run happened", not for every 0 in it.
//!
//! Our entropy coder is [`crate::ans`], which doesn't have that 1-bit
//! floor — read alone, it already prices a very likely 0 at a fraction of
//! a bit ([`crate::codecs::rans`] measured about 0.15 bits on an even more
//! skewed 90%-one-byte input). But a long run of identical bytes still
//! costs `run_length * that_fraction` bits under rANS, against
//! `O(log run_length)` for writing the length directly — worth keeping for
//! highly repetitive data. So we still split runs of 0 out, just into a
//! plain length instead of bzip2's extra alphabet symbols: every run
//! becomes one *token* (byte value 0, meaning "a run of 0s follows") plus
//! its length written separately as an Elias gamma code, and every other
//! byte becomes one token carrying its own value. The token stream — never
//! bigger than the input, at most 255 distinct values wide — then goes
//! through [`crate::ans`] exactly like [`crate::codecs::rans`]'s bytes do.
//!
//! This is **not** the `.bz2` file format: bzip2 also randomises pathological
//! blocks (a bug workaround from bzip1 kept for compatibility), splits
//! large Huffman-coded sections into several tables, and of course uses
//! Huffman rather than rANS. None of that changes what the transform
//! demonstrates, so we didn't chase bit-for-bit compatibility here the way
//! [`crate::deflate`]/[`crate::gzip`] and [`crate::lzma`] do for their
//! formats.
//!
//! Block layout, repeated until the input is exhausted (integers as Elias
//! gamma codes, LSB-first bits, byte-aligned before each raw section):
//!
//! ```text
//! gamma(primary_index + 1)     row of the original block in forward()'s sorted rotations
//! gamma(block_len + 1)         bytes of original data in this block
//! 256 × gamma(freq + 1)        token frequency table
//! gamma(token_count + 1)       number of tokens
//! gamma(coded_len + 1)         byte length of the rANS-coded tokens
//! gamma(run_lengths_bits + 1)  bit length of the run-length side channel
//! (pad to a byte)
//! [coded_len bytes]            rANS-coded tokens
//! [run_lengths bytes]          Elias gamma-coded run lengths, back to back
//! ```

use super::{forward, inverse, mtf};
use crate::ans::{self, Freqs};
use crate::bits::{BitReader, BitWriter};
use crate::{Error, Result};

/// Bytes of original data per block. Matches bzip2 `-9`'s maximum, a
/// reasonable trade-off between how far the transform can look and how
/// long the O(n log n) sort takes.
pub const BLOCK_SIZE: usize = 900_000;

pub fn compress(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for block in input.chunks(BLOCK_SIZE) {
        encode_block(block, &mut out);
    }
    out
}

pub fn decompress(input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_len);
    let mut pos = 0;
    while out.len() < expected_len {
        let rest = input.get(pos..).ok_or(Error::UnexpectedEof)?;
        pos += decode_block(rest, expected_len - out.len(), &mut out)?;
    }
    if pos != input.len() {
        return Err(Error::Corrupt("data after the last BWT block"));
    }
    Ok(out)
}

fn encode_block(block: &[u8], out: &mut Vec<u8>) {
    let n = block.len();
    let (l, primary) = forward(block);
    let ranks = mtf::encode(&l);
    let (tokens, run_lengths, run_lengths_bits) = split_runs(&ranks);

    let mut counts = [0u64; 256];
    for &t in &tokens {
        counts[usize::from(t)] += 1;
    }
    let freqs = Freqs::normalize(&counts).expect("a non-empty block always has at least one token");
    let coded = ans::encode(&tokens, &freqs);

    let mut w = BitWriter::with_capacity(300 + coded.len());
    w.write_gamma(u64::from(primary) + 1);
    w.write_gamma(n as u64 + 1);
    for &f in freqs.freqs() {
        w.write_gamma(u64::from(f) + 1);
    }
    w.write_gamma(tokens.len() as u64 + 1);
    w.write_gamma(coded.len() as u64 + 1);
    w.write_gamma(run_lengths_bits + 1);
    out.extend_from_slice(&w.finish());
    out.extend_from_slice(&coded);
    out.extend_from_slice(&run_lengths);
}

/// Returns bytes of `input` consumed.
fn decode_block(input: &[u8], remaining: usize, out: &mut Vec<u8>) -> Result<usize> {
    let mut r = BitReader::new(input);
    // `read_gamma` never decodes to 0 (the encoder never writes it: see
    // `write_gamma`'s doc), so subtracting 1 to undo our `+ 1` encoding of
    // possibly-zero fields can't underflow.
    let primary = r.read_gamma()? - 1;
    let n = usize::try_from(r.read_gamma()? - 1)
        .map_err(|_| Error::Corrupt("BWT block length too large"))?;
    if n == 0 || n > remaining {
        return Err(Error::Corrupt(
            "BWT block length invalid or larger than allowed",
        ));
    }
    let mut freq = [0u32; 256];
    for f in &mut freq {
        *f = u32::try_from(r.read_gamma()? - 1)
            .map_err(|_| Error::Corrupt("BWT token frequency too large"))?;
    }
    let token_count = usize::try_from(r.read_gamma()? - 1)
        .map_err(|_| Error::Corrupt("BWT token count too large"))?;
    // A token always accounts for at least one output byte (either a
    // literal, or a run of at least one 0), so it can never outnumber them.
    if token_count > n {
        return Err(Error::Corrupt("BWT token count exceeds block length"));
    }
    let coded_len = usize::try_from(r.read_gamma()? - 1)
        .map_err(|_| Error::Corrupt("BWT coded length too large"))?;
    let run_lengths_bits = r.read_gamma()? - 1;
    r.align_to_byte();
    let mut pos = r.byte_position();

    // `.get(pos..).and_then(|rest| rest.get(..len))` rather than
    // `.get(pos..pos + len)`: `len` comes straight from the (untrusted)
    // header and could overflow the addition before bounds-checking runs.
    let coded = input
        .get(pos..)
        .and_then(|rest| rest.get(..coded_len))
        .ok_or(Error::UnexpectedEof)?;
    pos += coded_len;
    let run_lengths_byte_len = run_lengths_bits.div_ceil(8) as usize;
    let run_lengths = input
        .get(pos..)
        .and_then(|rest| rest.get(..run_lengths_byte_len))
        .ok_or(Error::UnexpectedEof)?;
    pos += run_lengths_byte_len;

    let freqs = Freqs::from_freqs(freq)?;
    let mut tokens = Vec::new();
    ans::decode(coded, &freqs, token_count, &mut tokens)?;

    let ranks = join_runs(&tokens, run_lengths, n)?;
    let l = mtf::decode(&ranks);
    let primary =
        u32::try_from(primary).map_err(|_| Error::Corrupt("BWT primary index too large"))?;
    out.extend_from_slice(&inverse(&l, primary)?);
    Ok(pos)
}

/// Split `ranks` into a token per run of 0s (however long) or literal
/// non-zero byte, plus the run lengths as a separate Elias gamma bitstream
/// (in the order their tokens appear).
fn split_runs(ranks: &[u8]) -> (Vec<u8>, Vec<u8>, u64) {
    let mut tokens = Vec::new();
    let mut w = BitWriter::new();
    let mut i = 0;
    while i < ranks.len() {
        if ranks[i] == 0 {
            let start = i;
            while i < ranks.len() && ranks[i] == 0 {
                i += 1;
            }
            tokens.push(0);
            w.write_gamma((i - start) as u64);
        } else {
            tokens.push(ranks[i]);
            i += 1;
        }
    }
    let bits = w.bit_len();
    (tokens, w.finish(), bits)
}

/// Inverse of [`split_runs`]: rebuild the `n`-byte rank stream from
/// `tokens` and the run-length bitstream. Rejects malformed input (a run
/// past `n`, or fewer bytes decoded than `n`) instead of panicking.
fn join_runs(tokens: &[u8], run_lengths: &[u8], n: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    let mut r = BitReader::new(run_lengths);
    for &t in tokens {
        if t == 0 {
            let run = r.read_gamma()?;
            if run == 0 || out.len() as u64 + run > n as u64 {
                return Err(Error::Corrupt(
                    "BWT run length invalid or overruns the block",
                ));
            }
            out.resize(out.len() + run as usize, 0);
        } else {
            if out.len() >= n {
                return Err(Error::Corrupt("BWT token stream overruns the block"));
            }
            out.push(t);
        }
    }
    if out.len() != n {
        return Err(Error::Corrupt("BWT block decoded to the wrong length"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Codec;
    use crate::codecs::Huffman;
    use crate::stats::entropy_bits_per_byte;

    #[test]
    fn split_and_join_runs_roundtrip() {
        for ranks in [
            &[][..],
            &[0][..],
            &[1, 2, 0, 0, 0, 3][..],
            &[0; 500][..],
            &[5; 20][..],
        ] {
            let (tokens, lens, bits) = split_runs(ranks);
            assert!(bits <= lens.len() as u64 * 8);
            assert_eq!(join_runs(&tokens, &lens, ranks.len()).unwrap(), ranks);
        }
    }

    #[test]
    fn join_runs_rejects_malformed_input() {
        assert!(join_runs(&[0], &[], 5).is_err()); // no length to read
        assert!(join_runs(&[1, 1], &[], 1).is_err()); // token stream too long
        assert!(join_runs(&[1], &[], 2).is_err()); // decoded too short
    }

    #[test]
    fn beats_order0_entropy_on_structured_text() {
        // The BWT+MTF+RLE pipeline sees structure order-0 entropy can't:
        // repeated substrings, not just repeated bytes.
        let text = b"the quick brown fox jumps over the lazy dog. ".repeat(400);
        let packed = compress(&text);
        let ideal = entropy_bits_per_byte(&text) * text.len() as f64 / 8.0;
        assert!(
            (packed.len() as f64) < ideal * 0.5,
            "{} vs {ideal}",
            packed.len()
        );
        assert_eq!(decompress(&packed, text.len()).unwrap(), text);
    }

    #[test]
    fn beats_huffman_on_structured_text() {
        let text = b"abcabcabcabcabcabcabcabcabcabcabcabcxyzxyzxyzxyz".repeat(200);
        let packed = compress(&text);
        assert!(packed.len() * 4 < Huffman.compress(&text).len());
    }

    #[test]
    fn multi_block_roundtrip() {
        let mut data = vec![b'a'; BLOCK_SIZE];
        data.extend((0..=255u8).cycle().take(BLOCK_SIZE / 3 + 17));
        let packed = compress(&data);
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
        assert!(decompress(&packed, data.len() - 1).is_err());
    }
}
