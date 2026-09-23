//! LZMA encoder.
//!
//! Encoding has two halves: *parsing* decides which [`Op`]s describe the
//! input (literals, repeats of recent distances, new matches), and
//! [`Encoder`] codes them. Two parsers are available ([`Parse`]):
//!
//! - **Fast**: at each position, choose with rules of thumb from the LZMA
//!   SDK's fast mode (`GetOptimumFast` in `LzmaEnc.c`): repeats are cheap,
//!   so they win unless a new match is clearly longer; a match is skipped
//!   when the next position has a better one (lazy matching).
//! - **Optimal**: price every alternative in bits and pick the cheapest
//!   path through a whole stretch of input; see [`super::optimal`].

use super::matchfinder::{Match, MatchFinder};
use super::*;
use crate::range::{price_bit, price_tree};

/// How the encoder chooses what to code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parse {
    /// Rules of thumb, one position at a time. About 3× faster.
    Fast,
    /// Cheapest path by bit prices over up to 4 KB at a time.
    Optimal,
}

/// Encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Largest match distance, in bytes. More finds more repeats, and the
    /// decoder needs this much memory.
    pub dict_size: u32,
    /// Chain links the match finder follows per position.
    pub depth: usize,
    /// Accept a match this long without looking further.
    pub nice_len: usize,
    pub parse: Parse,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dict_size: 16 << 20,
            depth: 48,
            nice_len: 64,
            parse: Parse::Optimal,
        }
    }
}

/// Compress `input` to a `.lzma` stream with its size in the header and no
/// end marker.
pub fn compress(input: &[u8], opts: Options) -> Vec<u8> {
    let props = Props::default();
    // No point in a dictionary bigger than the input; decoders allocate it.
    let dict_size = opts
        .dict_size
        .min(input.len().next_power_of_two().min(1 << 31) as u32)
        .max(4096);

    let mut out = Vec::with_capacity(HEADER_LEN + input.len() / 3);
    out.push(props.to_byte());
    out.extend_from_slice(&dict_size.to_le_bytes());
    out.extend_from_slice(&(input.len() as u64).to_le_bytes());

    let mut enc = Encoder {
        data: input,
        rc: range::Encoder::with_capacity(input.len() / 3),
        m: Model::new(props),
        state: State::default(),
        reps: [0; 4],
    };
    let mut finder = MatchFinder::new(input, dict_size as usize, opts.depth, opts.nice_len);
    let nice_len = opts.nice_len.min(MATCH_MAX_LEN);

    match opts.parse {
        Parse::Fast => parse_fast(&mut enc, &mut finder, nice_len),
        Parse::Optimal => super::optimal::parse(&mut enc, &mut finder, nice_len),
    }
    out.extend_from_slice(&enc.rc.finish());
    out
}

fn parse_fast(enc: &mut Encoder, finder: &mut MatchFinder, nice_len: usize) {
    let mut matches = Vec::new();
    let mut next_matches = Vec::new();
    // `matches` already holds the search for `pos` from a lazy look-ahead.
    let mut have_matches = false;
    let mut pos = 0;
    while pos < enc.data.len() {
        if !have_matches {
            finder.find(pos, &mut matches);
        }
        have_matches = false;
        let op = match enc.choose(pos, &matches, finder, &mut next_matches, nice_len) {
            Choice::Literal => {
                // `choose` always searched pos + 1 before picking a
                // literal (unless this is the last byte), and it comes next.
                std::mem::swap(&mut matches, &mut next_matches);
                have_matches = true;
                enc.literal_or_short_rep(pos)
            }
            Choice::Rep { index, len } => Op::Rep { index, len },
            Choice::Match { dist, len } => Op::Match { dist, len },
        };
        enc.emit(pos, op);
        pos += op.len();
    }
}

/// One coded token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Op {
    #[default]
    Literal,
    /// One byte from distance `reps[0]`.
    ShortRep,
    /// `len` bytes from distance `reps[index]`.
    Rep { index: usize, len: usize },
    /// `len` bytes from a new distance (1-based).
    Match { dist: usize, len: usize },
}

impl Op {
    pub(super) fn len(self) -> usize {
        match self {
            Op::Literal | Op::ShortRep => 1,
            Op::Rep { len, .. } | Op::Match { len, .. } => len,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Literal,
    /// Reuse `reps[index]`.
    Rep {
        index: usize,
        len: usize,
    },
    /// New match; `dist` is 1-based.
    Match {
        dist: usize,
        len: usize,
    },
}

/// `big` is so much further than `small` that a match one byte shorter at
/// `small` is the better deal. (SDK's `ChangePair`, on 0-based distances.)
fn much_further(small: usize, big: usize) -> bool {
    (big - 1) >> 7 > small - 1
}

pub(super) struct Encoder<'a> {
    pub(super) data: &'a [u8],
    rc: range::Encoder,
    pub(super) m: Model,
    pub(super) state: State,
    /// Last four distances, 0-based as coded.
    pub(super) reps: [u32; 4],
}

impl Encoder<'_> {
    /// Decide what to code at `pos`, given the matches found there.
    /// May search `pos + 1` into `next` (only meaningful if the choice is a
    /// literal).
    fn choose(
        &self,
        pos: usize,
        matches: &[Match],
        finder: &mut MatchFinder,
        next: &mut Vec<Match>,
        nice_len: usize,
    ) -> Choice {
        next.clear();
        let avail = MATCH_MAX_LEN.min(self.data.len() - pos);
        if avail < MATCH_MIN_LEN {
            return self.lookahead_literal(pos, finder, next);
        }

        // The best repeat of a recent distance.
        let (mut rep_len, mut rep_index) = (0, 0);
        for (i, &r) in self.reps.iter().enumerate() {
            let len = finder.len_at(pos, r as usize + 1, avail);
            if len >= nice_len {
                return Choice::Rep { index: i, len };
            }
            if len > rep_len {
                (rep_len, rep_index) = (len, i);
            }
        }

        let (mut main_len, mut main_dist) = matches
            .last()
            .map_or((0, 0), |m| (m.len as usize, m.dist as usize));
        if main_len >= nice_len {
            return Choice::Match {
                dist: main_dist,
                len: main_len,
            };
        }
        if main_len >= MATCH_MIN_LEN {
            // Step down to a match one shorter if it is far closer.
            for pair in matches.windows(2).rev() {
                let (shorter, _) = (pair[0], pair[1]);
                if shorter.len as usize + 1 != main_len
                    || !much_further(shorter.dist as usize, main_dist)
                {
                    break;
                }
                main_len = shorter.len as usize;
                main_dist = shorter.dist as usize;
            }
            // A 2-byte match far away costs more than two literals.
            if main_len == 2 && main_dist > 0x80 {
                main_len = 1;
            }
        }

        if rep_len >= MATCH_MIN_LEN
            && (rep_len + 1 >= main_len
                || (rep_len + 2 >= main_len && main_dist > 1 << 9)
                || (rep_len + 3 >= main_len && main_dist > 1 << 15))
        {
            return Choice::Rep {
                index: rep_index,
                len: rep_len,
            };
        }
        if main_len < MATCH_MIN_LEN || avail <= 2 {
            return self.lookahead_literal(pos, finder, next);
        }

        // Lazy matching: is starting one byte later better?
        finder.find(pos + 1, next);
        if let Some(n) = next.last() {
            let (new_len, new_dist) = (n.len as usize, n.dist as usize);
            if (new_len >= main_len && new_dist < main_dist)
                || (new_len == main_len + 1 && !much_further(main_dist, new_dist))
                || new_len > main_len + 1
                || (new_len + 1 >= main_len && main_len >= 3 && much_further(new_dist, main_dist))
            {
                return Choice::Literal;
            }
        }
        // Or a repeat starting one byte later, nearly as long?
        let limit = (main_len - 1).max(2);
        for &r in &self.reps {
            if finder.len_at(pos + 1, r as usize + 1, limit) >= limit {
                return Choice::Literal;
            }
        }
        Choice::Match {
            dist: main_dist,
            len: main_len,
        }
    }

    /// Choose a literal, first searching `pos + 1` so the next step can
    /// reuse it.
    fn lookahead_literal(
        &self,
        pos: usize,
        finder: &mut MatchFinder,
        next: &mut Vec<Match>,
    ) -> Choice {
        if pos + 1 < self.data.len() {
            finder.find(pos + 1, next);
        }
        Choice::Literal
    }

    fn pos_state(&self, pos: usize) -> usize {
        self.m.pos_state(pos)
    }

    /// Code one byte as a literal or, when the byte at distance rep0 is
    /// the same and that is cheaper, as a "short rep".
    fn literal_or_short_rep(&mut self, pos: usize) -> Op {
        let byte = self.data[pos];
        let rep0 = self.reps[0] as usize + 1;
        if rep0 <= pos && self.data[pos - rep0] == byte {
            let s = self.state.index();
            let ps = self.pos_state(pos);
            let short_rep = price_bit(self.m.is_match[s][ps], 1)
                + price_bit(self.m.is_rep[s], 1)
                + price_bit(self.m.is_rep_g0[s], 0)
                + price_bit(self.m.is_rep0_long[s][ps], 0);
            let literal = price_bit(self.m.is_match[s][ps], 0) + self.literal_price(pos);
            if short_rep < literal {
                return Op::ShortRep;
            }
        }
        Op::Literal
    }

    /// Code `op` at `pos`.
    pub(super) fn emit(&mut self, pos: usize, op: Op) {
        match op {
            Op::Literal => self.literal(pos),
            Op::ShortRep => self.short_rep(pos),
            Op::Rep { index, len } => self.rep(pos, index, len),
            Op::Match { dist, len } => self.new_match(pos, dist, len),
        }
    }

    fn short_rep(&mut self, pos: usize) {
        let s = self.state.index();
        let ps = self.pos_state(pos);
        self.rc.encode_bit(&mut self.m.is_match[s][ps], 1);
        self.rc.encode_bit(&mut self.m.is_rep[s], 1);
        self.rc.encode_bit(&mut self.m.is_rep_g0[s], 0);
        self.rc.encode_bit(&mut self.m.is_rep0_long[s][ps], 0);
        self.state.after_short_rep();
    }

    fn prev_byte(&self, pos: usize) -> u8 {
        if pos == 0 { 0 } else { self.data[pos - 1] }
    }

    fn match_byte(&self, pos: usize) -> u8 {
        self.data[pos - self.reps[0] as usize - 1]
    }

    fn literal_price(&mut self, pos: usize) -> u32 {
        let byte = self.data[pos];
        let after_literal = self.state.is_literal();
        let match_byte = if after_literal {
            0
        } else {
            self.match_byte(pos)
        };
        let prev = self.prev_byte(pos);
        let probs = self.m.literal_probs(pos, prev);
        if after_literal {
            price_tree(probs, 8, u32::from(byte))
        } else {
            matched_literal(probs, byte, match_byte, |p, bit| price_bit(*p, bit))
        }
    }

    fn literal(&mut self, pos: usize) {
        let byte = self.data[pos];
        let s = self.state.index();
        let ps = self.pos_state(pos);
        self.rc.encode_bit(&mut self.m.is_match[s][ps], 0);
        let after_literal = self.state.is_literal();
        let match_byte = if after_literal {
            0
        } else {
            self.match_byte(pos)
        };
        let prev = self.prev_byte(pos);
        let probs = self.m.literal_probs(pos, prev);
        let rc = &mut self.rc;
        if after_literal {
            rc.encode_tree(probs, 8, u32::from(byte));
        } else {
            matched_literal(probs, byte, match_byte, |p, bit| {
                rc.encode_bit(p, bit);
                0
            });
        }
        self.state.after_literal();
    }

    fn rep(&mut self, pos: usize, index: usize, len: usize) {
        let s = self.state.index();
        let ps = self.pos_state(pos);
        let (rc, m) = (&mut self.rc, &mut self.m);
        rc.encode_bit(&mut m.is_match[s][ps], 1);
        rc.encode_bit(&mut m.is_rep[s], 1);
        if index == 0 {
            rc.encode_bit(&mut m.is_rep_g0[s], 0);
            rc.encode_bit(&mut m.is_rep0_long[s][ps], 1);
        } else {
            rc.encode_bit(&mut m.is_rep_g0[s], 1);
            if index == 1 {
                rc.encode_bit(&mut m.is_rep_g1[s], 0);
            } else {
                rc.encode_bit(&mut m.is_rep_g1[s], 1);
                rc.encode_bit(&mut m.is_rep_g2[s], (index - 2) as u32);
            }
            // Move the used distance to the front.
            self.reps[..=index].rotate_right(1);
        }
        m.rep_len.encode(rc, (len - MATCH_MIN_LEN) as u32, ps);
        self.state.after_rep();
    }

    fn new_match(&mut self, pos: usize, dist: usize, len: usize) {
        let s = self.state.index();
        let ps = self.pos_state(pos);
        let (rc, m) = (&mut self.rc, &mut self.m);
        rc.encode_bit(&mut m.is_match[s][ps], 1);
        rc.encode_bit(&mut m.is_rep[s], 0);
        let len = (len - MATCH_MIN_LEN) as u32;
        m.len.encode(rc, len, ps);
        let dist = (dist - 1) as u32;
        encode_dist(rc, m, dist, len);
        self.reps = [dist, self.reps[0], self.reps[1], self.reps[2]];
        self.state.after_match();
    }
}

/// Walk the bits of a literal coded against `match_byte` (see
/// `decode_matched`), calling `code` with each bit's probability. Returns
/// the sum of what `code` returns, so the same walk serves for coding and
/// for pricing.
fn matched_literal(
    probs: &mut [Prob],
    byte: u8,
    match_byte: u8,
    mut code: impl FnMut(&mut Prob, u32) -> u32,
) -> u32 {
    let mut total = 0;
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
        total += code(&mut probs[index], bit);
        symbol = (symbol << 1) | bit as usize;
    }
    total
}

/// Code a 0-based distance for a match of `len - 2`.
fn encode_dist(rc: &mut range::Encoder, m: &mut Model, dist: u32, len: u32) {
    let slot = dist_slot(dist);
    rc.encode_tree(&mut m.dist_slot[Model::len_state(len)], 6, slot);
    if slot < 4 {
        return;
    }
    let footer_bits = (slot >> 1) - 1;
    let base = (2 | (slot & 1)) << footer_bits;
    let rest = dist - base;
    if slot < END_POS_MODEL_INDEX {
        rc.encode_reverse_tree(
            &mut m.dist_special[(base - slot) as usize..],
            footer_bits,
            rest,
        );
    } else {
        rc.encode_direct(rest >> ALIGN_BITS, footer_bits - ALIGN_BITS);
        rc.encode_reverse_tree(
            &mut m.dist_align,
            ALIGN_BITS,
            rest & ((1 << ALIGN_BITS) - 1),
        );
    }
}
