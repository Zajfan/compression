//! Burrows–Wheeler transform: sort every rotation of a block and keep only
//! the last column.
//!
//! Take every rotation of `"banana"`:
//!
//! ```text
//! banana   ananab   nanaba   anaban   nabana   abanan
//! ```
//!
//! Sort them:
//!
//! ```text
//! abanan
//! anaban
//! ananab
//! banana   <- row 3: where the original string starts
//! nabana
//! nanaba
//! ```
//!
//! The transform's output is just the **last column**, `"nnbaaa"`, plus
//! **which row the original string ended up in** (3). Sorting brings rows
//! with the same few bytes before some position next to each other, and
//! since English (or any structured data) is full of repeated short
//! contexts ("th" is almost always followed by the same few letters), the
//! last column ends up with long runs of the same byte far more often than
//! the input did. That doesn't compress anything by itself: [`mtf`] and the
//! [pipeline](super::bwt) built on top turn those runs into something an
//! entropy coder can exploit.
//!
//! This is the transform bzip2 is built on, but nothing here matches the
//! `.bz2` file format — see [`super::bwt`] (the pipeline module) for that
//! trade-off.
//!
//! ## Sorting without a sentinel
//!
//! Textbook descriptions add a byte smaller than everything else to the end
//! of the string, so every rotation becomes a distinct, cleanly-ordered
//! suffix. We skip it (it would cost one byte, and finding a value not
//! already in the input isn't always possible): rotations are sorted
//! directly by treating the string as cyclic, and two rotations that are
//! genuinely identical (only possible in strings like `"aaaa"`) are broken
//! by whichever started at the lower position. [`sorted_rotations`] finds
//! this order in O(n log n): first sort by 1 byte, then by the first 2
//! (reusing the 1-byte sort as each half), then 4, doubling until every
//! rotation is distinguished from the rest.
//!
//! ## Undoing it: LF-mapping
//!
//! The last column alone determines the first column too (it's the same
//! bytes, sorted), and that's enough to walk backwards through the whole
//! matrix. For each row, `LF-mapping` finds the row whose first column
//! holds the byte that comes right before this row's: the row for the
//! matrix's `c`-th occurrence of byte `L[i]` sits at `count[bytes < L[i]] +
//! c`. Starting from the row the original string was in and repeatedly
//! following that mapping reads the input back one byte at a time,
//! backwards, in O(n).

pub mod mtf;
mod pipeline;

pub use pipeline::{BLOCK_SIZE, compress, decompress};

use crate::{Error, Result};

/// Transform `data`, returning the last column and the row the original
/// data ended up in after sorting (`< data.len()`, or 0 for empty input).
pub fn forward(data: &[u8]) -> (Vec<u8>, u32) {
    let n = data.len();
    if n <= 1 {
        return (data.to_vec(), 0);
    }
    let order = sorted_rotations(data);
    let primary = order
        .iter()
        .position(|&i| i == 0)
        .expect("rotation starting at 0 is always present") as u32;
    let l = order
        .iter()
        .map(|&i| data[(i as usize + n - 1) % n])
        .collect();
    (l, primary)
}

/// Undo [`forward`]. Fails if `primary_index` is out of range for `l`.
pub fn inverse(l: &[u8], primary_index: u32) -> Result<Vec<u8>> {
    let n = l.len();
    if n <= 1 {
        return Ok(l.to_vec());
    }
    if primary_index as usize >= n {
        return Err(Error::Corrupt("BWT primary index out of range"));
    }
    // `count[c]`: how many bytes in `l` are `< c` (`c`'s block starts here
    // in the sorted first column).
    let mut count = [0u32; 256];
    for &b in l {
        count[usize::from(b)] += 1;
    }
    let mut base = [0u32; 256];
    let mut acc = 0u32;
    for (b, &c) in count.iter().enumerate() {
        base[b] = acc;
        acc += c;
    }
    // LF-mapping: row `i`'s predecessor is the row where `l[i]` occurs, in
    // the same relative order it occurs in `l`.
    let mut seen = [0u32; 256];
    let mut lf = vec![0u32; n];
    for (i, &b) in l.iter().enumerate() {
        let b = usize::from(b);
        lf[i] = base[b] + seen[b];
        seen[b] += 1;
    }
    let mut out = vec![0u8; n];
    let mut row = primary_index as usize;
    for slot in out.iter_mut().rev() {
        *slot = l[row];
        row = lf[row] as usize;
    }
    Ok(out)
}

/// Order the rotations of `data` (given by their starting position) sort
/// before each other, cyclically. Ties (identical rotations) come out in
/// ascending order of starting position.
fn sorted_rotations(data: &[u8]) -> Vec<u32> {
    let n = data.len();
    let mut order = counting_sort(&(0..n as u32).collect::<Vec<_>>(), 256, |i| {
        u32::from(data[i as usize])
    });

    // `rank[i]`: 0-based position of the 1-byte string starting at `i`
    // among all of them, with ties (equal bytes) sharing a rank.
    let mut rank = vec![0u32; n];
    for w in order.windows(2) {
        let (prev, cur) = (w[0], w[1]);
        let bump = u32::from(data[cur as usize] != data[prev as usize]);
        rank[cur as usize] = rank[prev as usize] + bump;
    }

    // Double the compared length each round: `rank` describes the first
    // `k` bytes of each rotation, so `(rank[i], rank[i+k])` describes the
    // first `2k`.
    let mut k = 1usize;
    while k < n {
        let key2 = |i: u32| rank[(i as usize + k) % n];
        // Stable sort by the second half, then (stably) by the first: the
        // standard two-pass trick for sorting by a pair of keys.
        order = counting_sort(&order, n, key2);
        order = counting_sort(&order, n, |i| rank[i as usize]);

        let mut next_rank = vec![0u32; n];
        for w in order.windows(2) {
            let (prev, cur) = (w[0], w[1]);
            let same = rank[prev as usize] == rank[cur as usize] && key2(prev) == key2(cur);
            next_rank[cur as usize] = next_rank[prev as usize] + u32::from(!same);
        }
        rank = next_rank;
        if rank[*order.last().expect("n >= 1") as usize] as usize == n - 1 {
            break; // every rotation now has a distinct rank
        }
        k *= 2;
    }
    order
}

/// Stable counting sort of `source` by `key` (which must return values in
/// `0..buckets`).
fn counting_sort(source: &[u32], buckets: usize, key: impl Fn(u32) -> u32) -> Vec<u32> {
    let mut start = vec![0u32; buckets + 1];
    for &x in source {
        start[key(x) as usize + 1] += 1;
    }
    for i in 1..start.len() {
        start[i] += start[i - 1];
    }
    let mut out = vec![0u32; source.len()];
    for &x in source {
        let slot = &mut start[key(x) as usize];
        out[*slot as usize] = x;
        *slot += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn banana() {
        // Hand-derived from the sorted rotation matrix in the module docs.
        let (l, primary) = forward(b"banana");
        assert_eq!(l, b"nnbaaa");
        assert_eq!(primary, 3);
        assert_eq!(inverse(&l, primary).unwrap(), b"banana");
    }

    #[test]
    fn edge_cases_roundtrip() {
        for data in [
            &b""[..],
            b"a",
            b"aa",
            b"aaaa",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let (l, primary) = forward(data);
            assert_eq!(inverse(&l, primary).unwrap(), data, "{data:?}");
        }
    }

    #[test]
    fn bad_primary_index_is_rejected_not_panicked() {
        let (l, primary) = forward(b"banana");
        assert!(inverse(&l, l.len() as u32).is_err());
        assert!(inverse(&l, primary + 1000).is_err());
        assert!(inverse(&[], 0).unwrap().is_empty());
    }

    /// Sort rotations by literally building and sorting them, for small
    /// inputs where that's cheap. Cross-checks [`sorted_rotations`]'s
    /// doubling algorithm against a much simpler (and much slower) one.
    fn brute_force_bwt(data: &[u8]) -> (Vec<u8>, u32) {
        let n = data.len();
        if n <= 1 {
            // Matches forward()'s convention for 0- and 1-byte input: no
            // sorting needed, and there's nothing to pick a primary index
            // among.
            return (data.to_vec(), 0);
        }
        let mut rotations: Vec<(Vec<u8>, usize)> = (0..n)
            .map(|start| ((0..n).map(|j| data[(start + j) % n]).collect(), start))
            .collect();
        rotations.sort();
        let l = rotations.iter().map(|(rot, _)| rot[n - 1]).collect();
        let primary = rotations.iter().position(|(_, start)| *start == 0).unwrap();
        (l, primary as u32)
    }

    proptest! {
        #[test]
        fn matches_brute_force(data in proptest::collection::vec(0u8..4, 0..40)) {
            prop_assert_eq!(forward(&data), brute_force_bwt(&data));
        }

        #[test]
        fn roundtrips(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let (l, primary) = forward(&data);
            prop_assert_eq!(inverse(&l, primary).unwrap(), data);
        }

        #[test]
        fn decoder_never_panics_on_garbage(l in proptest::collection::vec(any::<u8>(), 0..256), primary: u32) {
            let _ = inverse(&l, primary);
        }
    }
}
