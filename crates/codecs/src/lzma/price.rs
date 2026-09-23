//! What each LZMA token would cost to code right now, in 1/16 bits.
//!
//! The optimal parser compares thousands of alternatives per position, so
//! the expensive parts (lengths and distances, which walk bit trees) are
//! read from tables. [`Prices`] snapshots them from the model; the encoder
//! refreshes it every so often as the model learns. The cheap parts (the
//! token-kind bits, and literals, whose context changes at every byte) are
//! priced directly.

use super::*;
use crate::range::{PRICE_ONE_BIT, price_bit, price_reverse_tree, price_tree};

/// Lengths 2..=273, as `len - 2`.
const LEN_SYMBOLS: usize = MATCH_MAX_LEN - MATCH_MIN_LEN + 1;

pub(super) struct Prices {
    len: Vec<[u32; LEN_SYMBOLS]>,
    rep_len: Vec<[u32; LEN_SYMBOLS]>,
    dist_slot: [[u32; 64]; LEN_TO_POS_STATES],
    /// Whole price of each distance below `NUM_FULL_DISTANCES`.
    full_dist: [[u32; NUM_FULL_DISTANCES as usize]; LEN_TO_POS_STATES],
    align: [u32; 1 << ALIGN_BITS],
}

impl Prices {
    pub(super) fn new(m: &Model) -> Self {
        let pos_states = 1 << m.props.pb;
        let mut p = Self {
            len: vec![[0; LEN_SYMBOLS]; pos_states],
            rep_len: vec![[0; LEN_SYMBOLS]; pos_states],
            dist_slot: [[0; 64]; LEN_TO_POS_STATES],
            full_dist: [[0; NUM_FULL_DISTANCES as usize]; LEN_TO_POS_STATES],
            align: [0; 1 << ALIGN_BITS],
        };
        p.update(m);
        p
    }

    /// Recompute every table from the model's current probabilities.
    pub(super) fn update(&mut self, m: &Model) {
        for (ps, (len, rep_len)) in self.len.iter_mut().zip(&mut self.rep_len).enumerate() {
            fill_len_prices(&m.len, ps, len);
            fill_len_prices(&m.rep_len, ps, rep_len);
        }
        for (ls, slots) in self.dist_slot.iter_mut().enumerate() {
            for (slot, price) in slots.iter_mut().enumerate() {
                *price = price_tree(&m.dist_slot[ls], 6, slot as u32);
            }
        }
        for (ls, full) in self.full_dist.iter_mut().enumerate() {
            for (dist, price) in full.iter_mut().enumerate() {
                let dist = dist as u32;
                let slot = dist_slot(dist);
                *price = self.dist_slot[ls][slot as usize];
                if slot >= 4 {
                    let footer_bits = (slot >> 1) - 1;
                    let base = (2 | (slot & 1)) << footer_bits;
                    *price += price_reverse_tree(
                        &m.dist_special[(base - slot) as usize..],
                        footer_bits,
                        dist - base,
                    );
                }
            }
        }
        for (v, price) in self.align.iter_mut().enumerate() {
            *price = price_reverse_tree(&m.dist_align, ALIGN_BITS, v as u32);
        }
    }

    /// Price of the length part of a new match.
    pub(super) fn len(&self, pos_state: usize, len: usize) -> u32 {
        self.len[pos_state][len - MATCH_MIN_LEN]
    }

    /// Price of the length part of a repeat match.
    pub(super) fn rep_len(&self, pos_state: usize, len: usize) -> u32 {
        self.rep_len[pos_state][len - MATCH_MIN_LEN]
    }

    /// Price of a 0-based distance for a match of length `len`.
    pub(super) fn dist(&self, dist: u32, len: usize) -> u32 {
        let ls = Model::len_state((len - MATCH_MIN_LEN) as u32);
        if dist < NUM_FULL_DISTANCES {
            return self.full_dist[ls][dist as usize];
        }
        let slot = dist_slot(dist);
        let footer_bits = (slot >> 1) - 1;
        self.dist_slot[ls][slot as usize]
            + (footer_bits - ALIGN_BITS) * PRICE_ONE_BIT
            + self.align[(dist & ((1 << ALIGN_BITS) - 1)) as usize]
    }
}

fn fill_len_prices(lm: &LenModel, ps: usize, out: &mut [u32; LEN_SYMBOLS]) {
    let (c0, c1) = (price_bit(lm.choice, 0), price_bit(lm.choice, 1));
    let (c2_0, c2_1) = (price_bit(lm.choice2, 0), price_bit(lm.choice2, 1));
    for (len, price) in out.iter_mut().enumerate() {
        let len = len as u32;
        *price = if len < 8 {
            c0 + price_tree(&lm.low[ps], 3, len)
        } else if len < 16 {
            c1 + c2_0 + price_tree(&lm.mid[ps], 3, len - 8)
        } else {
            c1 + c2_1 + price_tree(&lm.high, 8, len - 16)
        };
    }
}

/// Price of the byte at `pos` as a literal, including the is-match bit.
/// `match_byte` is the byte at distance rep0 when the last token was a
/// match (literals are then coded against it), `None` otherwise.
pub(super) fn literal(
    m: &Model,
    state: State,
    pos: usize,
    prev: u8,
    byte: u8,
    match_byte: Option<u8>,
) -> u32 {
    let base = m.literal_base(pos, prev);
    let probs = &m.literal[base..base + 0x300];
    let is_match = price_bit(m.is_match[state.index()][m.pos_state(pos)], 0);
    let Some(match_byte) = match_byte else {
        return is_match + price_tree(probs, 8, u32::from(byte));
    };
    let mut price = is_match;
    let mut symbol = 1usize;
    let mut same_so_far = true;
    for i in (0..8).rev() {
        let bit = u32::from(byte >> i) & 1;
        let index = if same_so_far {
            let match_bit = usize::from(match_byte >> i) & 1;
            same_so_far = match_bit == bit as usize;
            ((1 + match_bit) << 8) + symbol
        } else {
            symbol
        };
        price += price_bit(probs[index], bit);
        symbol = (symbol << 1) | bit as usize;
    }
    price
}

/// Price of a one-byte repeat of distance rep0.
pub(super) fn short_rep(m: &Model, state: State, pos_state: usize) -> u32 {
    let s = state.index();
    price_bit(m.is_match[s][pos_state], 1)
        + price_bit(m.is_rep[s], 1)
        + price_bit(m.is_rep_g0[s], 0)
        + price_bit(m.is_rep0_long[s][pos_state], 0)
}

/// Price of choosing repeat distance `index` (length not included).
pub(super) fn rep(m: &Model, index: usize, state: State, pos_state: usize) -> u32 {
    let s = state.index();
    let head = price_bit(m.is_match[s][pos_state], 1) + price_bit(m.is_rep[s], 1);
    head + match index {
        0 => price_bit(m.is_rep_g0[s], 0) + price_bit(m.is_rep0_long[s][pos_state], 1),
        1 => price_bit(m.is_rep_g0[s], 1) + price_bit(m.is_rep_g1[s], 0),
        _ => {
            price_bit(m.is_rep_g0[s], 1)
                + price_bit(m.is_rep_g1[s], 1)
                + price_bit(m.is_rep_g2[s], (index - 2) as u32)
        }
    }
}

/// Price of the token-kind bits of a new match.
pub(super) fn new_match(m: &Model, state: State, pos_state: usize) -> u32 {
    let s = state.index();
    price_bit(m.is_match[s][pos_state], 1) + price_bit(m.is_rep[s], 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_model_prices_are_bit_counts() {
        // With every probability at 1/2, a price is 16 per coded bit.
        let m = Model::new(Props::default());
        let p = Prices::new(&m);
        let one = PRICE_ONE_BIT;
        assert_eq!(p.len(0, 2), one + 3 * one);
        assert_eq!(p.len(0, 10), 2 * one + 3 * one);
        assert_eq!(p.len(0, 273), 2 * one + 8 * one);
        // Distance 0: 6 slot bits. Distance 1000 (slot 19, 8 footer bits):
        // 6 + 4 direct + 4 align.
        assert_eq!(p.dist(0, 2), 6 * one);
        assert_eq!(p.dist(1000, 5), (6 + 4 + 4) * one);
        assert_eq!(p.dist(100, 5), (6 + 5) * one);
        let s = State::default();
        assert_eq!(literal(&m, s, 0, 0, b'x', None), 9 * one);
        assert_eq!(literal(&m, s, 0, 0, b'x', Some(b'y')), 9 * one);
        assert_eq!(short_rep(&m, s, 0), 4 * one);
        assert_eq!(rep(&m, 3, s, 0), 5 * one);
        assert_eq!(new_match(&m, s, 0), 2 * one);
    }
}
