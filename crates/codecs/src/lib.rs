//! Compression algorithms, implemented from scratch.
//!
//! Every algorithm implements [`Codec`] and is listed in [`all_codecs`], which
//! automatically puts it under the shared round-trip tests, the benchmarks and
//! the CLI.

pub mod ans;
pub mod bits;
pub mod codecs;
pub mod crc32;
pub mod deflate;
pub mod error;
pub mod frame;
pub mod gzip;
pub mod huffman;
pub mod lz77;
pub mod lzma;
pub mod range;
pub mod stats;

pub use error::{Error, Result};

/// A lossless compression algorithm.
///
/// The one rule: for every input `x`,
/// `decompress(&compress(x)) == x`.
pub trait Codec: Send + Sync {
    /// Stable numeric id stored in frame headers. Never reuse or change one.
    fn id(&self) -> u8;

    /// Short lowercase name used on the command line, e.g. `"store"`.
    fn name(&self) -> &'static str;

    /// One-line description shown by `cmpr codecs`.
    fn description(&self) -> &'static str;

    /// Compress `input` into a new buffer.
    fn compress(&self, input: &[u8]) -> Vec<u8>;

    /// Decompress `input`. `expected_len` is the original size, used as a
    /// capacity hint and to reject corrupt data that would expand too far.
    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>>;
}

/// Every codec this crate provides, in id order.
pub fn all_codecs() -> Vec<Box<dyn Codec>> {
    vec![
        Box::new(codecs::Store),
        Box::new(codecs::PackBits),
        Box::new(codecs::Rle),
        Box::new(codecs::Huffman),
        Box::new(codecs::Lzss),
        Box::new(codecs::Deflate),
        Box::new(codecs::Range0),
        Box::new(codecs::Range1),
        Box::new(codecs::Rans),
        Box::new(codecs::Lzma),
    ]
}

/// Look up a codec by its command-line name.
pub fn codec_by_name(name: &str) -> Option<Box<dyn Codec>> {
    all_codecs().into_iter().find(|c| c.name() == name)
}

/// Look up a codec by the id stored in a frame header.
pub fn codec_by_id(id: u8) -> Option<Box<dyn Codec>> {
    all_codecs().into_iter().find(|c| c.id() == id)
}
