//! Every registered codec must pass these. Adding a codec to
//! `cmpr_codecs::all_codecs()` is enough to put it under test.

use cmpr_codecs::{all_codecs, codec_by_id, codec_by_name};
use cmpr_testkit::{arb_data, assert_standard_suite, check_roundtrip};
use proptest::prelude::*;
use std::collections::HashSet;

#[test]
fn standard_inputs_roundtrip() {
    for codec in all_codecs() {
        assert_standard_suite(codec.as_ref());
    }
}

#[test]
fn ids_and_names_are_unique_and_resolvable() {
    let codecs = all_codecs();
    let ids: HashSet<u8> = codecs.iter().map(|c| c.id()).collect();
    let names: HashSet<&str> = codecs.iter().map(|c| c.name()).collect();
    assert_eq!(ids.len(), codecs.len(), "duplicate codec id");
    assert_eq!(names.len(), codecs.len(), "duplicate codec name");
    for c in &codecs {
        assert_eq!(codec_by_id(c.id()).unwrap().name(), c.name());
        assert_eq!(codec_by_name(c.name()).unwrap().id(), c.id());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn random_data_roundtrips(data in arb_data()) {
        for codec in all_codecs() {
            if let Err(e) = check_roundtrip(codec.as_ref(), &data) {
                prop_assert!(false, "{}", e);
            }
        }
    }

    /// Decoders must reject garbage with an error, never panic or hang.
    #[test]
    fn decoders_survive_garbage(data in proptest::collection::vec(any::<u8>(), 0..2048), len in 0usize..4096) {
        for codec in all_codecs() {
            let _ = codec.decompress(&data, len);
        }
        let _ = cmpr_codecs::frame::decode(&data);
    }
}
