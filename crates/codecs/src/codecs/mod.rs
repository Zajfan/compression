//! One module per algorithm. Register new codecs in [`crate::all_codecs`].

mod deflate;
mod huffman;
mod lzss;
mod packbits;
mod rle;
mod store;

pub use deflate::Deflate;
pub use huffman::Huffman;
pub use lzss::Lzss;
pub use packbits::PackBits;
pub use rle::Rle;
pub use store::Store;
