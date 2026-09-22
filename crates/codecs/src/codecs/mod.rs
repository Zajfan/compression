//! One module per algorithm. Register new codecs in [`crate::all_codecs`].

mod deflate;
mod huffman;
mod lzss;
mod packbits;
mod range;
mod rans;
mod rle;
mod store;

pub use deflate::Deflate;
pub use huffman::Huffman;
pub use lzss::Lzss;
pub use packbits::PackBits;
pub use range::{Range0, Range1};
pub use rans::Rans;
pub use rle::Rle;
pub use store::Store;
