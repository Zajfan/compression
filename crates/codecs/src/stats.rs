//! Measurements of data, used to judge how well codecs do.

/// Order-0 Shannon entropy in bits per byte: the fewest bits per byte any
/// codec can achieve if it codes each byte independently of its neighbours.
///
/// 0.0 means every byte is the same; 8.0 means all 256 values are equally
/// common (random data). Codecs that use context or repetition (LZ77,
/// context mixing, ...) can go below this number; order-0 ones can't.
pub fn entropy_bits_per_byte(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let n = data.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Order-1 entropy in bits per byte: like [`entropy_bits_per_byte`], but
/// each byte is measured given the byte before it. The limit for codecs
/// that use one byte of context, such as `range1`. It is always at most the
/// order-0 value; the gap shows how much neighbouring bytes predict each
/// other.
pub fn order1_entropy_bits_per_byte(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    // counts[prev][byte]; the first byte gets context 0, as in `range1`.
    let mut counts = vec![[0u64; 256]; 256];
    let mut prev = 0usize;
    for &b in data {
        counts[prev][usize::from(b)] += 1;
        prev = usize::from(b);
    }
    let bits: f64 = counts
        .iter()
        .map(|row| {
            let n: u64 = row.iter().sum();
            row.iter()
                .filter(|&&c| c > 0)
                .map(|&c| c as f64 * (n as f64 / c as f64).log2())
                .sum::<f64>()
        })
        .sum();
    bits / data.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(entropy_bits_per_byte(b""), 0.0);
        assert_eq!(entropy_bits_per_byte(b"aaaa"), 0.0);
        assert!((entropy_bits_per_byte(b"abab") - 1.0).abs() < 1e-12);
        let all: Vec<u8> = (0..=255).collect();
        assert!((entropy_bits_per_byte(&all) - 8.0).abs() < 1e-12);
    }

    #[test]
    fn order1_sees_what_order0_cannot() {
        // Order-0 sees two equally common bytes, 1 bit each. Given the
        // previous byte, the next one is certain.
        let data = b"ab".repeat(1000);
        assert!((entropy_bits_per_byte(&data) - 1.0).abs() < 1e-12);
        assert!(order1_entropy_bits_per_byte(&data) < 0.001);
        assert_eq!(order1_entropy_bits_per_byte(b""), 0.0);
    }
}
