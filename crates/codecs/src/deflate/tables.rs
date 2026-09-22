//! Constant tables from RFC 1951 §3.2.5–3.2.7.

/// Smallest length for length codes 257..=285.
pub const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits after each length code.
pub const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Smallest distance for distance codes 0..=29.
pub const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits after each distance code.
pub const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Order in which code-length code lengths are sent (most likely first, so
/// trailing zeros can be dropped).
pub const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

pub const END_OF_BLOCK: usize = 256;
/// Literal/length symbols actually used (286 and 287 are reserved).
pub const NUM_LITLEN: usize = 286;
pub const NUM_DIST: usize = 30;

/// A length or distance split into (code index, extra bits value, extra bit count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coded {
    pub index: usize,
    pub extra: u32,
    pub extra_bits: u32,
}

fn split(value: usize, base: &[u16], extra: &[u8]) -> Coded {
    let index = base.partition_point(|&b| usize::from(b) <= value) - 1;
    Coded {
        index,
        extra: (value - usize::from(base[index])) as u32,
        extra_bits: u32::from(extra[index]),
    }
}

/// Length 3..=258 → index into the length codes (symbol = 257 + index).
pub fn length_code(len: usize) -> Coded {
    split(len, &LENGTH_BASE, &LENGTH_EXTRA)
}

/// Distance 1..=32768 → distance code.
pub fn dist_code(dist: usize) -> Coded {
    split(dist, &DIST_BASE, &DIST_EXTRA)
}

/// Code lengths of the fixed literal/length code (block type 1).
pub fn fixed_litlen_lengths() -> [u8; 288] {
    let mut l = [0u8; 288];
    l[..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..].fill(8);
    l
}

/// Code lengths of the fixed distance code: 5 bits each.
pub fn fixed_dist_lengths() -> [u8; NUM_DIST] {
    [5; NUM_DIST]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(index: usize, extra: u32, extra_bits: u32) -> Coded {
        Coded {
            index,
            extra,
            extra_bits,
        }
    }

    #[test]
    fn length_codes() {
        assert_eq!(length_code(3), c(0, 0, 0));
        assert_eq!(length_code(12), c(8, 1, 1)); // 11 + 1
        assert_eq!(length_code(257), c(27, 30, 5)); // 227 + 30
        // 258 has its own code with no extra bits.
        assert_eq!(length_code(258), c(28, 0, 0));
    }

    #[test]
    fn dist_codes() {
        assert_eq!(dist_code(1), c(0, 0, 0));
        assert_eq!(dist_code(6), c(4, 1, 1)); // 5 + 1
        assert_eq!(dist_code(32768), c(29, 8191, 13)); // 24577 + 8191
    }

    #[test]
    fn every_value_roundtrips() {
        for len in 3..=258 {
            let c = length_code(len);
            assert!(c.extra < 1 << c.extra_bits || c.extra_bits == 0 && c.extra == 0);
            assert_eq!(usize::from(LENGTH_BASE[c.index]) + c.extra as usize, len);
        }
        for dist in 1..=32768 {
            let c = dist_code(dist);
            assert!(c.extra < 1 << c.extra_bits || c.extra_bits == 0 && c.extra == 0);
            assert_eq!(usize::from(DIST_BASE[c.index]) + c.extra as usize, dist);
        }
    }
}
