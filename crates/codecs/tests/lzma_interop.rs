//! Our LZMA must be compatible with the real thing. `liblzma` (the library
//! inside xz) is the reference: it must decode what we write, and we must
//! decode what it writes, including other lc/lp/pb settings and its
//! end-marker streams.

use cmpr_codecs::lzma::{Options, Parse, compress, decompress};
use cmpr_testkit::{arb_data, standard_inputs};
use liblzma::read::XzDecoder;
use liblzma::stream::{LzmaOptions, Stream};
use liblzma::write::XzEncoder;
use proptest::prelude::*;
use std::io::{Read, Write};

fn reference_decode(data: &[u8]) -> Vec<u8> {
    let stream = Stream::new_lzma_decoder(u64::MAX).unwrap();
    let mut out = Vec::new();
    XzDecoder::new_stream(data, stream)
        .read_to_end(&mut out)
        .expect("liblzma rejected our stream");
    out
}

/// liblzma's `.lzma` encoder: size unknown in the header, end marker at
/// the end.
fn reference_encode(data: &[u8], opts: &LzmaOptions) -> Vec<u8> {
    let stream = Stream::new_lzma_encoder(opts).unwrap();
    let mut enc = XzEncoder::new_stream(Vec::new(), stream);
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn check_both_ways(data: &[u8]) -> Result<(), TestCaseError> {
    for parse in [Parse::Optimal, Parse::Fast] {
        let ours = compress(
            data,
            Options {
                parse,
                ..Options::default()
            },
        );
        prop_assert_eq!(
            reference_decode(&ours),
            data,
            "liblzma decoding ours ({:?})",
            parse
        );
    }
    let theirs = reference_encode(data, &LzmaOptions::new_preset(6).unwrap());
    let decoded =
        decompress(&theirs, usize::MAX).map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(decoded, data, "us decoding liblzma");
    Ok(())
}

#[test]
fn standard_inputs_both_ways() {
    for (name, data) in standard_inputs() {
        check_both_ways(&data).unwrap_or_else(|e| panic!("[{name}] {e}"));
    }
}

#[test]
fn large_varied_input() {
    let mut data = Vec::new();
    for i in 0..60_000u32 {
        data.extend_from_slice(format!("row {} value {}\n", i, i * 7919 % 1000).as_bytes());
    }
    check_both_ways(&data).unwrap();
}

#[test]
fn other_literal_and_position_settings() {
    let data = b"LZMA props test \x00\x01\x02\x03 with some binary-ish data. ".repeat(300);
    // liblzma only encodes lc + lp <= 4.
    for (lc, lp, pb) in [(0, 0, 0), (4, 0, 0), (0, 4, 4), (1, 2, 3), (2, 2, 1)] {
        let mut opts = LzmaOptions::new_preset(6).unwrap();
        opts.literal_context_bits(lc)
            .literal_position_bits(lp)
            .position_bits(pb);
        let theirs = reference_encode(&data, &opts);
        assert_eq!(
            decompress(&theirs, usize::MAX).unwrap(),
            data,
            "lc={lc} lp={lp} pb={pb}"
        );
    }
}

#[test]
fn output_limit_is_enforced() {
    let theirs = reference_encode(&[0u8; 100_000], &LzmaOptions::new_preset(6).unwrap());
    assert!(decompress(&theirs, 99_999).is_err());
    let ours = compress(&[0u8; 100_000], Options::default());
    assert!(decompress(&ours, 99_999).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn random_data_both_ways(data in arb_data()) {
        check_both_ways(&data)?;
    }

    #[test]
    fn decoder_never_panics_on_garbage(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = decompress(&data, 1 << 20);
    }
}
