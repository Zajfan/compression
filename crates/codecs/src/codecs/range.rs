//! Adaptive range coding of bytes, with no context (order-0) or with the
//! previous byte as context (order-1).
//!
//! Unlike [`Huffman`](super::Huffman) there is no code table in the output.
//! Encoder and decoder start from the same "know nothing" probabilities and
//! update them the same way after every bit, so the decoder rebuilds the
//! model as it goes. That is what "adaptive" means: the statistics are free.
//!
//! Each byte is coded as 8 binary decisions down a 256-leaf tree
//! ([`crate::range::Encoder::encode_tree`]).
//!
//! - **order-0** has one tree, so it learns how common each byte is. On
//!   uniform data it lands a few percent above the order-0 entropy, where
//!   Huffman is hard to beat. Real files change as they go, and there it
//!   wins clearly because it keeps adapting: on Canterbury plus part of
//!   Silesia it is 11% smaller than Huffman, and even below the static
//!   order-0 entropy.
//! - **order-1** has one tree per previous byte (256 trees). After a `q` it
//!   learns to expect `u`; after a space, the start of a word. That breaks
//!   through the order-0 limit that Huffman is stuck at.
//!
//! Stream layout: the range coder's bytes. Empty input gives empty output.

use crate::range::{self, PROB_INIT, Prob};
use crate::{Codec, Result};

/// Probabilities for one 8-bit tree.
const TREE: usize = 256;

fn compress(input: &[u8], contexts: usize) -> Vec<u8> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut probs: Vec<Prob> = vec![PROB_INIT; contexts * TREE];
    let mut enc = range::Encoder::with_capacity(input.len() / 2);
    let mut prev = 0usize;
    for &b in input {
        let ctx = (prev % contexts) * TREE;
        enc.encode_tree(&mut probs[ctx..ctx + TREE], 8, u32::from(b));
        prev = usize::from(b);
    }
    enc.finish()
}

fn decompress(input: &[u8], expected_len: usize, contexts: usize) -> Result<Vec<u8>> {
    if expected_len == 0 {
        return if input.is_empty() {
            Ok(Vec::new())
        } else {
            Err(crate::Error::Corrupt("data for an empty input"))
        };
    }
    let mut probs: Vec<Prob> = vec![PROB_INIT; contexts * TREE];
    let mut dec = range::Decoder::new(input)?;
    let mut out = Vec::with_capacity(expected_len);
    let mut prev = 0usize;
    for _ in 0..expected_len {
        let ctx = (prev % contexts) * TREE;
        let b = dec.decode_tree(&mut probs[ctx..ctx + TREE], 8) as u8;
        out.push(b);
        prev = usize::from(b);
    }
    dec.finish()?;
    Ok(out)
}

/// Order-0 adaptive range coder.
#[derive(Debug, Clone, Copy, Default)]
pub struct Range0;

impl Codec for Range0 {
    fn id(&self) -> u8 {
        6
    }

    fn name(&self) -> &'static str {
        "range0"
    }

    fn description(&self) -> &'static str {
        "Adaptive binary range coder, order-0 (no context), LZMA-style"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        compress(input, 1)
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        decompress(input, expected_len, 1)
    }
}

/// Order-1 adaptive range coder: the previous byte picks the model.
#[derive(Debug, Clone, Copy, Default)]
pub struct Range1;

impl Codec for Range1 {
    fn id(&self) -> u8 {
        7
    }

    fn name(&self) -> &'static str {
        "range1"
    }

    fn description(&self) -> &'static str {
        "Adaptive binary range coder, order-1 (previous byte as context)"
    }

    fn compress(&self, input: &[u8]) -> Vec<u8> {
        compress(input, 256)
    }

    fn decompress(&self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        decompress(input, expected_len, 256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::Huffman;
    use crate::stats::{entropy_bits_per_byte, order1_entropy_bits_per_byte};

    fn text() -> Vec<u8> {
        let mut t = Vec::new();
        for i in 0..400 {
            t.extend_from_slice(b"the quick brown fox jumps over the lazy dog; ");
            t.extend_from_slice(i.to_string().as_bytes());
            t.extend_from_slice(b" quiet queens quarrel quietly. ");
        }
        t
    }

    #[test]
    fn order0_is_close_to_entropy_on_uniform_text() {
        // A fixed code suits text whose statistics never change, so here
        // Huffman is hard to beat: the adaptive estimate keeps jittering
        // around the true probabilities and pays a few percent for it.
        let t = text();
        let packed = Range0.compress(&t).len() as f64;
        let ideal = entropy_bits_per_byte(&t) * t.len() as f64 / 8.0;
        assert!(packed < ideal * 1.08, "{packed} vs {ideal}");
    }

    #[test]
    fn order0_beats_huffman_when_the_data_changes() {
        // Text followed by a table of numbers. Huffman has to use one code
        // for both halves; the adaptive model re-learns in the middle.
        let mut data = text();
        for i in 0..20_000u32 {
            data.extend_from_slice(format!("{},", i * 7919 % 10_007).as_bytes());
        }
        let packed = Range0.compress(&data).len();
        let huffman = Huffman.compress(&data).len();
        assert!(packed * 10 < huffman * 9, "{packed} vs huffman {huffman}");
    }

    #[test]
    fn order1_breaks_the_order0_limit() {
        let t = text();
        let o0 = entropy_bits_per_byte(&t) * t.len() as f64 / 8.0;
        let o1 = order1_entropy_bits_per_byte(&t) * t.len() as f64 / 8.0;
        let packed = Range1.compress(&t).len() as f64;
        assert!(packed < o0 * 0.7, "{packed} vs order-0 {o0}");
        // On an input this small, learning 256 separate models from scratch
        // costs a lot over the order-1 ideal, which assumes they are known.
        assert!(packed < o1 * 1.4, "{packed} vs order-1 {o1}");
    }

    #[test]
    fn constant_input_is_nearly_free() {
        let packed = Range0.compress(&[7u8; 100_000]);
        // 8 bits per byte at the probability floor (31/2048) costs about
        // 0.18 bits per byte.
        assert!(packed.len() < 2_500, "{}", packed.len());
    }
}
