//! Deflate (RFC 1951): LZ77 + Huffman, the format inside zip, gzip and png.
//!
//! A Deflate stream is a series of blocks. Each block starts with 3 bits:
//! "is this the last block?" and the block type:
//!
//! | type | meaning |
//! |------|---------|
//! | 0 | **stored**: raw bytes, for data that won't compress |
//! | 1 | **fixed Huffman**: LZ77 tokens with codes defined by the spec |
//! | 2 | **dynamic Huffman**: LZ77 tokens with codes sent in the block header |
//!
//! Literals and match lengths share one Huffman alphabet (0–255 literal
//! bytes, 256 end-of-block, 257–285 lengths), and distances have a second
//! one (0–29). Long lengths and distances are grouped into ranges: the code
//! picks the range and a few raw "extra bits" pick the value inside it.
//!
//! Spec: <https://www.rfc-editor.org/rfc/rfc1951>

mod decode;
mod encode;
mod tables;

pub use decode::{Inflated, inflate};
pub use encode::deflate;

/// Default compression level, same as zlib.
pub const DEFAULT_LEVEL: u8 = 6;
