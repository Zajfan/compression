//! Move-to-front: turn "which byte" into "how long ago did we last see a
//! byte this rare".
//!
//! Keep a list of all 256 byte values, most recently used first. To encode
//! a byte, output its position in the list, then move it to the front. A
//! byte seen very recently (including "the same byte as last time", which
//! [`super`]'s sorted rotations produce long runs of) always encodes as a
//! small number; a byte that hasn't appeared in a while encodes as a large
//! one. The output says nothing about *which* bytes are common overall,
//! only about local repetition, which is exactly what an entropy coder
//! downstream can't see on its own.
//!
//! Finding a byte's position and shifting everything before it is O(256)
//! per byte here; a smarter structure (e.g. a small balanced tree) would
//! make it O(log 256), a possible later optimisation.

/// `data` with each byte replaced by its move-to-front rank.
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut table: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        let rank = table
            .iter()
            .position(|&x| x == b)
            .expect("every byte value is in the table");
        out.push(rank as u8);
        table.copy_within(0..rank, 1);
        table[0] = b;
    }
    out
}

/// Inverse of [`encode`].
pub fn decode(ranks: &[u8]) -> Vec<u8> {
    let mut table: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut out = Vec::with_capacity(ranks.len());
    for &rank in ranks {
        let rank = usize::from(rank);
        let b = table[rank];
        out.push(b);
        table.copy_within(0..rank, 1);
        table[0] = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn repeats_encode_as_zero() {
        assert_eq!(encode(b"aaaa"), [b'a', 0, 0, 0]);
    }

    #[test]
    fn first_sight_costs_its_initial_table_position() {
        // The table starts as byte values in order, so the very first
        // byte of the input always costs exactly its own value.
        assert_eq!(encode(b"z")[0], b'z');
        assert_eq!(encode(&[0])[0], 0);
    }

    proptest! {
        #[test]
        fn roundtrips(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            prop_assert_eq!(decode(&encode(&data)), data);
        }
    }
}
