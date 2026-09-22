//! One module per algorithm. Register new codecs in [`crate::all_codecs`].

mod huffman;
mod packbits;
mod rle;
mod store;

pub use huffman::Huffman;
pub use packbits::PackBits;
pub use rle::Rle;
pub use store::Store;
