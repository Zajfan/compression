//! Optimal parsing: the cheapest way, in bits, to code a stretch of input.
//!
//! The fast parser decides one position at a time with rules of thumb. But
//! the choices interact: taking a match changes where the next token starts,
//! which matches are available there, and which distances are cheap to
//! repeat. This parser looks at a whole stretch of input instead, as a
//! shortest-path problem:
//!
//! ```text
//! node i = "the first i bytes of the stretch are coded"
//! edge   = one token: literal (i -> i+1), repeat or match of length L (i -> i+L)
//! weight = that token's price in bits, from the current model
//! ```
//!
//! Positions are visited in order. The cheapest way to reach node `i` is
//! final once every node before it has been expanded, because edges only go
//! forward. Each node remembers how it was reached, so the winning path can
//! be traced back from the end.
//!
//! A token's price depends on the state machine and the four repeat
//! distances, which depend on the path taken so far. Each node therefore
//! carries the state and distances of the cheapest path into it. (That
//! makes this a very good heuristic rather than a true optimum, since a
//! pricier path with more useful repeat distances is dropped. The LZMA SDK
//! makes the same trade-off.)
//!
//! Keeping only the cheapest arrival at each node loses paths that are a
//! few bits dearer but keep a useful repeat distance. The most common case
//! is text where a line repeats with one character changed: match, literal,
//! then the same distance again. So, like the SDK, we also add combined
//! edges priced from the node's own distances: literal + rep0, and
//! (rep or match) + literal + rep0. On repetitive text that was the
//! difference between losing to the fast parser and beating it.
//!
//! A stretch ends where every path meets (no token from before crosses the
//! node), after 4 KB, or when a match of `nice_len` or more appears. That
//! match is then taken as is, which keeps long repetitive runs cheap to
//! encode.
//!
//! Adapted from `GetOptimum` in the LZMA SDK's `LzmaEnc.c`.

use super::encode::{Encoder, Op};
use super::matchfinder::{Match, MatchFinder};
use super::price::{self, Prices};
use super::*;

/// Most positions in one stretch.
const OPT_LEN: usize = 1 << 12;
/// Refresh the length and distance price tables after this many tokens.
const PRICE_REFRESH: usize = 64;

/// The tokens on one edge.
#[derive(Debug, Clone, Copy)]
enum Step {
    One(Op),
    /// `first` (if any), then a literal, then rep0 for `rep_len` bytes.
    LitRep0 {
        first: Option<Op>,
        rep_len: usize,
    },
}

impl Default for Step {
    fn default() -> Self {
        Step::One(Op::Literal)
    }
}

impl Step {
    /// The ops, last first.
    fn ops_reversed(self) -> impl Iterator<Item = Op> {
        let (a, b, c) = match self {
            Step::One(op) => (Some(op), None, None),
            Step::LitRep0 { first, rep_len } => (
                Some(Op::Rep {
                    index: 0,
                    len: rep_len,
                }),
                Some(Op::Literal),
                first,
            ),
        };
        [a, b, c].into_iter().flatten()
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Node {
    price: u32,
    /// Node this one was reached from, and with which tokens.
    prev: u32,
    step: Step,
    /// State machine and repeat distances after reaching this node on its
    /// cheapest path.
    state: State,
    reps: [u32; 4],
}

pub(super) fn parse(enc: &mut Encoder, finder: &mut MatchFinder, nice_len: usize) {
    let mut prices = Prices::new(&enc.m);
    let mut since_refresh = 0;
    // Room for the longest edge (match + literal + rep) past the last node.
    let mut nodes = vec![Node::default(); OPT_LEN + 2 * MATCH_MAX_LEN + 2];
    let mut matches = Vec::new();
    let mut path = Vec::new();
    // `matches` already holds the search for `pos`.
    let mut have_matches = false;
    let mut pos = 0;
    while pos < enc.data.len() {
        if since_refresh >= PRICE_REFRESH {
            prices.update(&enc.m);
            since_refresh = 0;
        }
        if !have_matches {
            finder.find(pos, &mut matches);
        }
        have_matches = stretch(
            enc,
            &prices,
            finder,
            pos,
            &mut matches,
            &mut nodes,
            &mut path,
            nice_len,
        );
        for &op in &path {
            enc.emit(pos, op);
            pos += op.len();
            since_refresh += 1;
        }
    }
}

/// Find the cheapest ops for the stretch starting at `pos` and put them in
/// `path`. `matches` holds the matches at `pos`. Returns true if it now
/// holds the matches at the end of the stretch.
#[allow(clippy::too_many_arguments)]
fn stretch(
    enc: &Encoder,
    prices: &Prices,
    finder: &mut MatchFinder,
    pos: usize,
    matches: &mut Vec<Match>,
    nodes: &mut [Node],
    path: &mut Vec<Op>,
    nice_len: usize,
) -> bool {
    let data = enc.data;
    let m = &enc.m;
    path.clear();

    // A long enough repeat or match at the start: take it, no search.
    let avail = MATCH_MAX_LEN.min(data.len() - pos);
    if let Some(op) = long_op(finder, pos, &enc.reps, matches, avail, nice_len) {
        path.push(op);
        return false;
    }

    nodes[0] = Node {
        price: 0,
        prev: 0,
        step: Step::default(),
        state: enc.state,
        reps: enc.reps,
    };
    // Nodes 1..=len_end have been reached.
    let mut len_end = 0;
    let mut cur = 0;
    let mut carried = false;
    loop {
        if cur > 0 {
            if cur == len_end || cur >= OPT_LEN {
                break;
            }
            let from = nodes[nodes[cur].prev as usize];
            let (state, reps) = nodes[cur]
                .step
                .ops_reversed()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .fold((from.state, from.reps), |(s, r), op| apply(s, r, op));
            nodes[cur].state = state;
            nodes[cur].reps = reps;

            finder.find(pos + cur, matches);
            let avail = MATCH_MAX_LEN.min(data.len() - pos - cur);
            if long_op(finder, pos + cur, &reps, matches, avail, nice_len).is_some() {
                // Stop here; the next stretch starts with it.
                carried = true;
                break;
            }
        }

        let node = nodes[cur];
        let p = pos + cur;
        let ps = m.pos_state(p);
        let avail = MATCH_MAX_LEN.min(data.len() - p);
        let mut relax = |to: usize, price: u32, step: Step| {
            let to = cur + to;
            while len_end < to {
                len_end += 1;
                nodes[len_end].price = u32::MAX;
            }
            if price < nodes[to].price {
                nodes[to].price = price;
                nodes[to].prev = cur as u32;
                nodes[to].step = step;
            }
        };

        // Literal, and the same byte as a short rep.
        let byte = data[p];
        let prev = if p == 0 { 0 } else { data[p - 1] };
        let rep0 = node.reps[0] as usize + 1;
        let match_byte = (!node.state.is_literal()).then(|| data[p - rep0]);
        let literal = price::literal(m, node.state, p, prev, byte, match_byte);
        relax(1, node.price + literal, Step::One(Op::Literal));
        if rep0 <= p && data[p - rep0] == byte {
            let short_rep = price::short_rep(m, node.state, ps);
            relax(1, node.price + short_rep, Step::One(Op::ShortRep));
        }
        let ctx = Ctx {
            data,
            m,
            prices,
            finder: &*finder,
        };
        if let Some((to, price, step)) = ctx.lit_rep0(p, node.price, node.state, node.reps, None) {
            relax(to, price, step);
        }
        if avail >= MATCH_MIN_LEN {
            // Repeats of the four recent distances, every length.
            for (index, &r) in node.reps.iter().enumerate() {
                let len = finder.len_at(p, r as usize + 1, avail);
                if len < MATCH_MIN_LEN {
                    continue;
                }
                let base = node.price + price::rep(m, index, node.state, ps);
                for l in MATCH_MIN_LEN..=len {
                    let op = Op::Rep { index, len: l };
                    relax(l, base + prices.rep_len(ps, l), Step::One(op));
                }
                let op = Op::Rep { index, len };
                let price = base + prices.rep_len(ps, len);
                if let Some(e) = ctx.lit_rep0(p, price, node.state, node.reps, Some(op)) {
                    relax(e.0, e.1, e.2);
                }
            }
            // New matches: each distance serves the lengths above the
            // previous (closer) one's.
            let base = node.price + price::new_match(m, node.state, ps);
            let mut from_len = MATCH_MIN_LEN;
            for mt in matches.iter() {
                let dist = mt.dist as usize;
                let mut price = 0;
                for l in from_len..=mt.len as usize {
                    price = base + prices.len(ps, l) + prices.dist(dist as u32 - 1, l);
                    relax(l, price, Step::One(Op::Match { dist, len: l }));
                }
                if mt.len as usize >= from_len {
                    let op = Op::Match {
                        dist,
                        len: mt.len as usize,
                    };
                    if let Some(e) = ctx.lit_rep0(p, price, node.state, node.reps, Some(op)) {
                        relax(e.0, e.1, e.2);
                    }
                }
                from_len = mt.len as usize + 1;
            }
        }
        cur += 1;
    }

    // Trace the cheapest path back from `cur`.
    let mut i = cur;
    while i > 0 {
        path.extend(nodes[i].step.ops_reversed());
        i = nodes[i].prev as usize;
    }
    path.reverse();
    carried
}

/// What pricing a combined edge needs.
struct Ctx<'a> {
    data: &'a [u8],
    m: &'a Model,
    prices: &'a Prices,
    finder: &'a MatchFinder<'a>,
}

impl Ctx<'_> {
    /// The edge "`first` (if any) from `p`, a literal, then rep0", given
    /// the state, distances and price at `p` and the price including
    /// `first`. Returns (length, price, step) if rep0 then matches at least
    /// 2 bytes.
    fn lit_rep0(
        &self,
        p: usize,
        price: u32,
        state: State,
        reps: [u32; 4],
        first: Option<Op>,
    ) -> Option<(usize, u32, Step)> {
        let data = self.data;
        let (state, reps, first_len) = match first {
            Some(op) => {
                let (s, r) = apply(state, reps, op);
                (s, r, op.len())
            }
            None => (state, reps, 0),
        };
        let lit_pos = p + first_len;
        let rep0 = reps[0] as usize + 1;
        // The literal must differ from the byte at rep0; if not, rep0 alone
        // would have covered it.
        if lit_pos + 1 + MATCH_MIN_LEN > data.len()
            || rep0 > lit_pos
            || data[lit_pos] == data[lit_pos - rep0]
        {
            return None;
        }
        let avail = MATCH_MAX_LEN.min(data.len() - lit_pos - 1);
        let rep_len = self.finder.len_at(lit_pos + 1, rep0, avail);
        if rep_len < MATCH_MIN_LEN {
            return None;
        }
        let match_byte = (!state.is_literal()).then(|| data[lit_pos - rep0]);
        let prev = data[lit_pos - 1];
        let literal = price::literal(self.m, state, lit_pos, prev, data[lit_pos], match_byte);
        let mut after = state;
        after.after_literal();
        let ps = self.m.pos_state(lit_pos + 1);
        let rep = price::rep(self.m, 0, after, ps) + self.prices.rep_len(ps, rep_len);
        Some((
            first_len + 1 + rep_len,
            price + literal + rep,
            Step::LitRep0 { first, rep_len },
        ))
    }
}

/// A repeat or match of at least `nice_len` at `pos`, if there is one.
fn long_op(
    finder: &MatchFinder,
    pos: usize,
    reps: &[u32; 4],
    matches: &[Match],
    avail: usize,
    nice_len: usize,
) -> Option<Op> {
    if avail < MATCH_MIN_LEN {
        return None;
    }
    let (mut best_len, mut best_index) = (0, 0);
    for (index, &r) in reps.iter().enumerate() {
        let len = finder.len_at(pos, r as usize + 1, avail);
        if len > best_len {
            (best_len, best_index) = (len, index);
        }
    }
    if best_len >= nice_len {
        return Some(Op::Rep {
            index: best_index,
            len: best_len,
        });
    }
    let m = matches.last()?;
    (m.len as usize >= nice_len).then_some(Op::Match {
        dist: m.dist as usize,
        len: m.len as usize,
    })
}

/// State and repeat distances after coding `op`, mirroring the encoder.
fn apply(mut state: State, mut reps: [u32; 4], op: Op) -> (State, [u32; 4]) {
    match op {
        Op::Literal => state.after_literal(),
        Op::ShortRep => state.after_short_rep(),
        Op::Rep { index, .. } => {
            reps[..=index].rotate_right(1);
            state.after_rep();
        }
        Op::Match { dist, .. } => {
            reps = [dist as u32 - 1, reps[0], reps[1], reps[2]];
            state.after_match();
        }
    }
    (state, reps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lzma::{Options, Parse, compress, decompress};

    fn text() -> Vec<u8> {
        let mut t = Vec::new();
        for i in 0..3000u32 {
            let line = format!(
                "{} the {} brown fox jumps over the {} dog, {} times.\n",
                i % 97,
                ["quick", "slow", "lazy", "sleepy"][i as usize % 4],
                ["lazy", "happy", "quick"][i as usize % 3],
                i * 7 % 13
            );
            t.extend_from_slice(line.as_bytes());
        }
        t
    }

    #[test]
    fn optimal_beats_fast_and_roundtrips() {
        let data = text();
        let size = |parse| {
            let packed = compress(
                &data,
                Options {
                    parse,
                    ..Options::default()
                },
            );
            assert_eq!(decompress(&packed, data.len()).unwrap(), data, "{parse:?}");
            packed.len()
        };
        let (fast, optimal) = (size(Parse::Fast), size(Parse::Optimal));
        assert!(
            optimal * 100 < fast * 97,
            "optimal {optimal} vs fast {fast}"
        );
    }

    #[test]
    fn long_runs_and_stretch_limits() {
        // Long runs trigger the nice_len shortcut; varied text longer than
        // OPT_LEN forces stretches to be cut at the limit.
        let mut data = vec![b'a'; 10_000];
        data.extend((0..20_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 27) as u8 + b'a'));
        data.extend(text());
        let packed = compress(&data, Options::default());
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
    }

    #[test]
    fn apply_mirrors_the_decoder() {
        let reps = [10, 20, 30, 40];
        let s = State::default();
        assert_eq!(
            apply(s, reps, Op::Rep { index: 2, len: 5 }).1,
            [30, 10, 20, 40]
        );
        assert_eq!(apply(s, reps, Op::Rep { index: 0, len: 5 }).1, reps);
        assert_eq!(
            apply(s, reps, Op::Match { dist: 100, len: 5 }).1,
            [99, 10, 20, 30]
        );
        assert_eq!(apply(s, reps, Op::ShortRep), (State(9), reps));
    }
}
