//! Shared round-trip checks for codecs.
//!
//! A codec is correct when `decompress(compress(x)) == x` for every `x`. This
//! crate tries hard to find an `x` where that fails: hand-picked edge cases
//! ([`standard_inputs`]) plus randomly generated data ([`arb_data`]).

use cmpr_codecs::{Codec, frame};
use proptest::prelude::*;

/// Compress then decompress `data`, and describe the first difference if the
/// result does not match.
pub fn check_roundtrip(codec: &dyn Codec, data: &[u8]) -> Result<(), String> {
    let name = codec.name();
    let compressed = codec.compress(data);
    let restored = codec.decompress(&compressed, data.len()).map_err(|e| {
        format!(
            "{name}: decompress failed on {} input bytes: {e}",
            data.len()
        )
    })?;
    if restored.len() != data.len() {
        return Err(format!(
            "{name}: length changed: {} bytes in, {} bytes out",
            data.len(),
            restored.len()
        ));
    }
    if let Some(i) = data.iter().zip(&restored).position(|(a, b)| a != b) {
        return Err(format!(
            "{name}: first difference at byte {i}: expected {:#04x}, got {:#04x}",
            data[i], restored[i]
        ));
    }
    let framed = frame::decode(&frame::encode(codec, data))
        .map_err(|e| format!("{name}: frame round-trip failed: {e}"))?;
    if framed != data {
        return Err(format!("{name}: frame round-trip changed the data"));
    }
    Ok(())
}

/// Deterministic pseudo-random bytes (xorshift), so failures are reproducible.
pub fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Edge cases every codec must handle. Each has a name for error messages.
pub fn standard_inputs() -> Vec<(&'static str, Vec<u8>)> {
    let text = b"It was the best of times, it was the worst of times, it was the age of \
                 wisdom, it was the age of foolishness, it was the epoch of belief. "
        .repeat(64);
    vec![
        ("empty", vec![]),
        ("one byte", vec![b'a']),
        ("two bytes", b"ab".to_vec()),
        ("all zeros 64K", vec![0; 1 << 16]),
        ("all 0xFF 64K", vec![0xFF; 1 << 16]),
        ("every byte value", (0..=255).collect()),
        (
            "every byte value x16",
            (0..=255).cycle().take(4096).collect(),
        ),
        ("alternating", [0xAA, 0x55].repeat(5000)),
        ("english text", text),
        ("random 1K", pseudo_random(1 << 10, 1)),
        ("random 256K", pseudo_random(1 << 18, 2)),
        ("long run then random", {
            let mut v = vec![b'x'; 100_000];
            v.extend(pseudo_random(1000, 3));
            v
        }),
        ("just over 64K", pseudo_random((1 << 16) + 1, 4)),
    ]
}

/// Run every standard input through `codec`, panicking with all failures.
pub fn assert_standard_suite(codec: &dyn Codec) {
    let failures: Vec<String> = standard_inputs()
        .iter()
        .filter_map(|(label, data)| {
            check_roundtrip(codec, data)
                .err()
                .map(|e| format!("[{label}] {e}"))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "round-trip failures:\n{}",
        failures.join("\n")
    );
}

/// Random inputs shaped like real data: pure noise, long runs, small
/// alphabets, and repeated chunks (which exercise match finders).
pub fn arb_data() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..4096),
        proptest::collection::vec((any::<u8>(), 1..300usize), 0..40).prop_map(|runs| runs
            .into_iter()
            .flat_map(|(b, n)| std::iter::repeat_n(b, n))
            .collect()),
        proptest::collection::vec(0u8..4, 0..8192),
        (proptest::collection::vec(any::<u8>(), 1..64), 1..200usize)
            .prop_map(|(chunk, times)| chunk.repeat(times)),
    ]
}
