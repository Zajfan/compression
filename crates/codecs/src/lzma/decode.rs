//! LZMA decoder.

use super::*;
use crate::lz77::copy_match;
use crate::{Error, Result};

/// Decode a `.lzma` stream (header included).
///
/// Fails if the output would exceed `limit` bytes, so a small malicious
/// stream can't expand into gigabytes.
pub fn decompress(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let header = input.get(..HEADER_LEN).ok_or(Error::UnexpectedEof)?;
    let props = Props::from_byte(header[0])?;
    // header[1..5] is the dictionary size: how far back matches may reach.
    // We keep the whole output in memory, so it only matters as a check
    // done by the distance test below.
    let size = u64::from_le_bytes(header[5..13].try_into().expect("8 bytes"));
    let known_size = (size != u64::MAX).then_some(size);
    if let Some(size) = known_size {
        if size > limit as u64 {
            return Err(Error::Corrupt("LZMA output larger than allowed"));
        }
    }
    let limit = known_size.map_or(limit, |s| s as usize);

    let mut rc = range::Decoder::new(&input[HEADER_LEN..])?;
    let mut m = Model::new(props);
    // Don't trust the header for a big allocation up front.
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(input.len().saturating_mul(4)));
    let mut state = State::default();
    let mut reps = [0u32; 4];

    loop {
        if known_size.is_some_and(|s| out.len() as u64 == s) {
            // An end marker may still follow a stream of known size.
            if rc.position() >= input.len() - HEADER_LEN {
                break;
            }
        }
        if rc.overrun() {
            return Err(Error::UnexpectedEof);
        }
        let pos = out.len();
        let pos_state = m.pos_state(pos);

        if rc.decode_bit(&mut m.is_match[state.index()][pos_state]) == 0 {
            if pos >= limit {
                return Err(Error::Corrupt("LZMA output larger than allowed"));
            }
            let prev = out.last().copied().unwrap_or(0);
            let lit_state = state;
            let probs = m.literal_probs(pos, prev);
            let byte = if lit_state.is_literal() {
                rc.decode_tree(probs, 8) as u8
            } else {
                let match_byte = out[pos - reps[0] as usize - 1];
                decode_matched(&mut rc, probs, match_byte)
            };
            out.push(byte);
            state.after_literal();
            continue;
        }

        let len;
        if rc.decode_bit(&mut m.is_rep[state.index()]) == 0 {
            // New distance.
            len = m.len.decode(&mut rc, pos_state);
            state.after_match();
            let dist = decode_dist(&mut rc, &mut m, len);
            if dist == END_MARKER_DIST {
                break;
            }
            reps = [dist, reps[0], reps[1], reps[2]];
        } else {
            // One of the last four distances.
            if rc.decode_bit(&mut m.is_rep_g0[state.index()]) == 0 {
                if rc.decode_bit(&mut m.is_rep0_long[state.index()][pos_state]) == 0 {
                    // "Short rep": one byte from distance rep0.
                    if reps[0] as usize >= pos || pos >= limit {
                        return Err(Error::Corrupt("bad LZMA short rep"));
                    }
                    state.after_short_rep();
                    out.push(out[pos - reps[0] as usize - 1]);
                    continue;
                }
            } else {
                let dist;
                if rc.decode_bit(&mut m.is_rep_g1[state.index()]) == 0 {
                    dist = reps[1];
                } else {
                    if rc.decode_bit(&mut m.is_rep_g2[state.index()]) == 0 {
                        dist = reps[2];
                    } else {
                        dist = reps[3];
                        reps[3] = reps[2];
                    }
                    reps[2] = reps[1];
                }
                reps[1] = reps[0];
                reps[0] = dist;
            }
            len = m.rep_len.decode(&mut rc, pos_state);
            state.after_rep();
        }

        let dist = reps[0] as usize + 1;
        let len = len as usize + MATCH_MIN_LEN;
        if dist > pos {
            return Err(Error::Corrupt("LZMA match distance before start of data"));
        }
        if pos + len > limit {
            return Err(Error::Corrupt("LZMA output larger than allowed"));
        }
        copy_match(&mut out, dist, len);
    }

    if known_size.is_some_and(|s| out.len() as u64 != s) {
        return Err(Error::Corrupt("LZMA end marker before the stated size"));
    }
    rc.finish()?;
    Ok(out)
}

/// A literal right after a match, coded against `match_byte`, the byte the
/// match would have continued with. While the bits agree, each bit gets
/// models that also depend on the match byte's bit; after the first
/// difference it continues as a plain literal.
fn decode_matched(rc: &mut range::Decoder, probs: &mut [Prob], match_byte: u8) -> u8 {
    let mut symbol = 1usize;
    let mut match_byte = u32::from(match_byte);
    while symbol < 0x100 {
        let match_bit = ((match_byte >> 7) & 1) as usize;
        match_byte <<= 1;
        let bit = rc.decode_bit(&mut probs[((1 + match_bit) << 8) + symbol]) as usize;
        symbol = (symbol << 1) | bit;
        if match_bit != bit {
            break;
        }
    }
    while symbol < 0x100 {
        symbol = (symbol << 1) | rc.decode_bit(&mut probs[symbol]) as usize;
    }
    symbol as u8
}

/// Decode a 0-based distance for a match of `len - 2`.
fn decode_dist(rc: &mut range::Decoder, m: &mut Model, len: u32) -> u32 {
    let slot = rc.decode_tree(&mut m.dist_slot[Model::len_state(len)], 6);
    if slot < 4 {
        return slot;
    }
    let footer_bits = (slot >> 1) - 1;
    let base = (2 | (slot & 1)) << footer_bits;
    if slot < END_POS_MODEL_INDEX {
        let probs = &mut m.dist_special[(base - slot) as usize..];
        base + rc.decode_reverse_tree(probs, footer_bits)
    } else {
        let high = rc.decode_direct(footer_bits - ALIGN_BITS) << ALIGN_BITS;
        base.wrapping_add(high)
            .wrapping_add(rc.decode_reverse_tree(&mut m.dist_align, ALIGN_BITS))
    }
}
