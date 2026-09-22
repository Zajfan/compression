//! Our Deflate and gzip must be compatible with the real thing. `flate2`
//! (backed by miniz_oxide, a zlib port) is the reference: it must decode
//! what we write, and we must decode what it writes.

use cmpr_codecs::deflate::{deflate, inflate};
use cmpr_codecs::gzip;
use cmpr_testkit::{arb_data, standard_inputs};
use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::{DeflateEncoder, GzEncoder};
use proptest::prelude::*;
use std::io::{Read, Write};

fn reference_inflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    DeflateDecoder::new(data)
        .read_to_end(&mut out)
        .expect("zlib rejected our stream");
    out
}

fn reference_deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn check_both_ways(data: &[u8], level: u8) -> Result<(), TestCaseError> {
    let ours = deflate(data, level);
    prop_assert_eq!(
        reference_inflate(&ours),
        data,
        "zlib decoding our level {} output",
        level
    );
    let theirs = reference_deflate(data, u32::from(level));
    let decoded = inflate(&theirs, data.len()).map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(
        decoded.data,
        data,
        "us decoding zlib level {} output",
        level
    );
    prop_assert_eq!(decoded.consumed, theirs.len());
    Ok(())
}

#[test]
fn standard_inputs_all_levels() {
    for (name, data) in standard_inputs() {
        for level in 0..=9 {
            check_both_ways(&data, level).unwrap_or_else(|e| panic!("[{name}] {e}"));
        }
    }
}

#[test]
fn large_varied_input() {
    // Big enough for many blocks and long distances.
    let mut data = Vec::new();
    for i in 0..40_000u32 {
        data.extend_from_slice(format!("line {} value {}\n", i, i * 7919 % 1000).as_bytes());
    }
    check_both_ways(&data, 6).unwrap();
    check_both_ways(&data, 9).unwrap();
}

#[test]
fn gzip_interop() {
    let data = b"gzip interop test. gzip interop test. gzip interop test.".repeat(100);
    let mut out = Vec::new();
    GzDecoder::new(&gzip::compress(&data, 6)[..])
        .read_to_end(&mut out)
        .unwrap();
    assert_eq!(out, data);

    // Reference file with a file name and comment in the header.
    let mut enc = flate2::GzBuilder::new()
        .filename("example.txt")
        .comment("made by flate2")
        .write(Vec::new(), Compression::best());
    enc.write_all(&data).unwrap();
    let file = enc.finish().unwrap();
    assert_eq!(gzip::decompress(&file, usize::MAX).unwrap(), data);

    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(b"").unwrap();
    assert_eq!(
        gzip::decompress(&enc.finish().unwrap(), usize::MAX).unwrap(),
        b""
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn random_data_both_ways(data in arb_data(), level in 0u8..=9) {
        check_both_ways(&data, level)?;
    }

    #[test]
    fn inflate_never_panics_on_garbage(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = inflate(&data, 1 << 20);
        let _ = gzip::decompress(&data, 1 << 20);
    }
}
