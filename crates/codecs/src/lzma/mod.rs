//! LZMA, the method behind 7-Zip and `.xz`: LZ77 with a large window,
//! coded bit by bit with the adaptive range coder ([`crate::range`]) and
//! many small context models.
//!
//! Deflate codes LZ77 tokens with Huffman tables fixed per block. LZMA
//! instead predicts every bit of every token from what came just before it:
//!
//! - **A 12-state machine** remembers the last few token kinds (literal,
//!   match, repeated match...). "Is the next token a match?" is predicted
//!   separately for each state and for the position's low bits, so the
//!   models learn things like "after a match, a literal is likely".
//! - **Repeated distances.** The last four distances are remembered, and
//!   reusing one costs a few bits instead of a whole distance. Tables and
//!   structs, where the same stride keeps coming back, gain a lot.
//! - **Literal contexts.** A literal is predicted from the high bits of the
//!   byte before it. Right after a match it is also compared bit by bit with
//!   the byte the match would have continued with (the "match byte"),
//!   because the literal usually differs from it in only a few bits.
//! - **A large window** (16 MB here, against Deflate's 32 KB) finds repeats
//!   that are far apart.
//!
//! The stream is the classic `.lzma` format ("LZMA_Alone"), readable by
//! `xz --format=lzma`, 7-Zip and liblzma:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 1 | properties: `(pb * 5 + lp) * 9 + lc` |
//! | 1 | 4 | dictionary size, little-endian |
//! | 5 | 8 | uncompressed size, little-endian (`u64::MAX` = unknown, end marker used) |
//! | 13 | .. | range-coded data |
//!
//! Spec: `lzma-specification.txt` and `LzmaDec.c` in the public-domain LZMA
//! SDK by Igor Pavlov.

mod decode;
mod encode;
mod matchfinder;
mod optimal;
mod price;

pub use decode::decompress;
pub use encode::{Finder, Options, Parse, compress};

use crate::range::{self, PROB_INIT, Prob};

/// Size of the `.lzma` header.
pub const HEADER_LEN: usize = 13;

const NUM_STATES: usize = 12;
/// States below this follow a literal; from here on they follow a match.
const LIT_STATES: u8 = 7;
const POS_STATES_MAX: usize = 1 << 4;
const MATCH_MIN_LEN: usize = 2;
const MATCH_MAX_LEN: usize = MATCH_MIN_LEN + 8 + 8 + 256 - 1; // 273
/// Distance-slot models, chosen by match length: 2, 3, 4, 5+.
const LEN_TO_POS_STATES: usize = 4;
/// Slots from here on send their middle bits directly, not modelled.
const END_POS_MODEL_INDEX: u32 = 14;
const NUM_FULL_DISTANCES: u32 = 1 << (END_POS_MODEL_INDEX >> 1);
const ALIGN_BITS: u32 = 4;
/// Distance value that marks the end of the stream.
const END_MARKER_DIST: u32 = u32::MAX;

/// Literal context and position bits. We always write lc=3, lp=0, pb=2,
/// the LZMA defaults; the decoder accepts any valid combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Props {
    /// High bits of the previous byte used as literal context (0–8).
    pub lc: u32,
    /// Low bits of the position used as literal context (0–4).
    pub lp: u32,
    /// Low bits of the position used as token context (0–4).
    pub pb: u32,
}

impl Default for Props {
    fn default() -> Self {
        Self {
            lc: 3,
            lp: 0,
            pb: 2,
        }
    }
}

impl Props {
    fn to_byte(self) -> u8 {
        ((self.pb * 5 + self.lp) * 9 + self.lc) as u8
    }

    fn from_byte(b: u8) -> crate::Result<Self> {
        let b = u32::from(b);
        if b >= 9 * 5 * 5 {
            return Err(crate::Error::Corrupt("invalid LZMA properties byte"));
        }
        Ok(Self {
            lc: b % 9,
            lp: b / 9 % 5,
            pb: b / 45,
        })
    }
}

/// The token-kind history. 0–6 mean the last token was a literal (with
/// different histories before it), 7–11 that it was some kind of match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct State(u8);

impl State {
    fn is_literal(self) -> bool {
        self.0 < LIT_STATES
    }

    fn after_literal(&mut self) {
        self.0 = match self.0 {
            0..=3 => 0,
            4..=9 => self.0 - 3,
            _ => self.0 - 6,
        };
    }

    fn after_match(&mut self) {
        self.0 = if self.is_literal() { 7 } else { 10 };
    }

    fn after_rep(&mut self) {
        self.0 = if self.is_literal() { 8 } else { 11 };
    }

    fn after_short_rep(&mut self) {
        self.0 = if self.is_literal() { 9 } else { 11 };
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Models for a match length, stored as `len - 2` in `0..=271`:
///
/// ```text
/// 0 + 3 bits   0–7     (per position state)
/// 10 + 3 bits  8–15    (per position state)
/// 11 + 8 bits  16–271
/// ```
#[derive(Clone)]
struct LenModel {
    choice: Prob,
    choice2: Prob,
    low: [[Prob; 1 << 3]; POS_STATES_MAX],
    mid: [[Prob; 1 << 3]; POS_STATES_MAX],
    high: [Prob; 1 << 8],
}

impl LenModel {
    fn new() -> Self {
        Self {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; POS_STATES_MAX],
            mid: [[PROB_INIT; 8]; POS_STATES_MAX],
            high: [PROB_INIT; 256],
        }
    }

    fn encode(&mut self, rc: &mut range::Encoder, len: u32, pos_state: usize) {
        if len < 8 {
            rc.encode_bit(&mut self.choice, 0);
            rc.encode_tree(&mut self.low[pos_state], 3, len);
        } else if len < 16 {
            rc.encode_bit(&mut self.choice, 1);
            rc.encode_bit(&mut self.choice2, 0);
            rc.encode_tree(&mut self.mid[pos_state], 3, len - 8);
        } else {
            rc.encode_bit(&mut self.choice, 1);
            rc.encode_bit(&mut self.choice2, 1);
            rc.encode_tree(&mut self.high, 8, len - 16);
        }
    }

    fn decode(&mut self, rc: &mut range::Decoder, pos_state: usize) -> u32 {
        if rc.decode_bit(&mut self.choice) == 0 {
            rc.decode_tree(&mut self.low[pos_state], 3)
        } else if rc.decode_bit(&mut self.choice2) == 0 {
            8 + rc.decode_tree(&mut self.mid[pos_state], 3)
        } else {
            16 + rc.decode_tree(&mut self.high, 8)
        }
    }
}

/// Every probability LZMA uses. Encoder and decoder each keep one and
/// update it identically.
#[derive(Clone)]
struct Model {
    props: Props,
    /// `0x300` probabilities per literal context: 256 for a plain literal
    /// tree, plus 2 × 256 for coding it against a match byte.
    literal: Vec<Prob>,
    is_match: [[Prob; POS_STATES_MAX]; NUM_STATES],
    is_rep: [Prob; NUM_STATES],
    is_rep_g0: [Prob; NUM_STATES],
    is_rep_g1: [Prob; NUM_STATES],
    is_rep_g2: [Prob; NUM_STATES],
    is_rep0_long: [[Prob; POS_STATES_MAX]; NUM_STATES],
    dist_slot: [[Prob; 1 << 6]; LEN_TO_POS_STATES],
    /// Reverse bit trees for the low bits of distances with slots 4–13.
    dist_special: [Prob; 1 + (NUM_FULL_DISTANCES - END_POS_MODEL_INDEX) as usize],
    /// Reverse bit tree for the lowest 4 bits of large distances.
    dist_align: [Prob; 1 << ALIGN_BITS],
    len: LenModel,
    rep_len: LenModel,
}

impl Model {
    fn new(props: Props) -> Self {
        Self {
            props,
            literal: vec![PROB_INIT; 0x300 << (props.lc + props.lp)],
            is_match: [[PROB_INIT; POS_STATES_MAX]; NUM_STATES],
            is_rep: [PROB_INIT; NUM_STATES],
            is_rep_g0: [PROB_INIT; NUM_STATES],
            is_rep_g1: [PROB_INIT; NUM_STATES],
            is_rep_g2: [PROB_INIT; NUM_STATES],
            is_rep0_long: [[PROB_INIT; POS_STATES_MAX]; NUM_STATES],
            dist_slot: [[PROB_INIT; 64]; LEN_TO_POS_STATES],
            dist_special: [PROB_INIT; 1 + (NUM_FULL_DISTANCES - END_POS_MODEL_INDEX) as usize],
            dist_align: [PROB_INIT; 1 << ALIGN_BITS],
            len: LenModel::new(),
            rep_len: LenModel::new(),
        }
    }

    fn pos_state(&self, pos: usize) -> usize {
        pos & ((1 << self.props.pb) - 1)
    }

    /// Where the 0x300 literal probabilities for a byte at `pos` after
    /// `prev` start.
    fn literal_base(&self, pos: usize, prev: u8) -> usize {
        let Props { lc, lp, .. } = self.props;
        let ctx = ((pos & ((1 << lp) - 1)) << lc) + (usize::from(prev) >> (8 - lc));
        0x300 * ctx
    }

    /// The 0x300 literal probabilities for a byte at `pos` after `prev`.
    fn literal_probs(&mut self, pos: usize, prev: u8) -> &mut [Prob] {
        let base = self.literal_base(pos, prev);
        &mut self.literal[base..base + 0x300]
    }

    /// Which models the distance slot uses, from `len - 2`.
    fn len_state(len: u32) -> usize {
        (len as usize).min(LEN_TO_POS_STATES - 1)
    }
}

/// Distance slot of a 0-based distance: 0–3 as is, then two slots per
/// power of two (the top two bits of the distance).
fn dist_slot(dist: u32) -> u32 {
    if dist < 4 {
        dist
    } else {
        let top = 31 - dist.leading_zeros();
        (top << 1) | ((dist >> (top - 1)) & 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dist_slots() {
        assert_eq!(dist_slot(0), 0);
        assert_eq!(dist_slot(3), 3);
        assert_eq!(dist_slot(4), 4);
        assert_eq!(dist_slot(5), 4);
        assert_eq!(dist_slot(6), 5);
        assert_eq!(dist_slot(127), 13);
        assert_eq!(dist_slot(128), 14);
        assert_eq!(dist_slot(u32::MAX), 63);
    }

    #[test]
    fn props_byte() {
        assert_eq!(Props::default().to_byte(), 0x5D);
        for b in 0..225 {
            assert_eq!(Props::from_byte(b).unwrap().to_byte(), b);
        }
        assert!(Props::from_byte(225).is_err());
    }

    #[test]
    fn state_machine_matches_the_spec() {
        // kLiteralNextStates / kMatchNextStates / kRepNextStates /
        // kShortRepNextStates from LzmaEnc.c.
        let lit = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5];
        for (s, &want) in lit.iter().enumerate() {
            let mut st = State(s as u8);
            st.after_literal();
            assert_eq!(st.0, want, "literal from {s}");
            let (mut m, mut r, mut sr) = (State(s as u8), State(s as u8), State(s as u8));
            m.after_match();
            r.after_rep();
            sr.after_short_rep();
            let after_lit = s < 7;
            assert_eq!(m.0, if after_lit { 7 } else { 10 });
            assert_eq!(r.0, if after_lit { 8 } else { 11 });
            assert_eq!(sr.0, if after_lit { 9 } else { 11 });
        }
    }
}
