//! CRC-32 (IEEE 802.3), the checksum used by zip, gzip and png.
//!
//! Table-driven, one byte at a time. The table is built at compile time.
//! Reference: <https://en.wikipedia.org/wiki/Cyclic_redundancy_check>

/// Reversed form of the polynomial x^32 + x^26 + x^23 + ... + x + 1.
const POLY: u32 = 0xEDB8_8320;

const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// Incremental CRC-32 for data that arrives in pieces.
#[derive(Debug, Clone, Copy)]
pub struct Crc32 {
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Self { state: !0 }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        for &byte in data {
            crc = TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
        self.state = crc;
    }

    pub const fn finish(&self) -> u32 {
        !self.state
    }
}

/// CRC-32 of a whole buffer.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0);
        // The standard CRC "check" value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data = b"hello, incremental world";
        let mut c = Crc32::new();
        c.update(&data[..5]);
        c.update(&data[5..]);
        assert_eq!(c.finish(), crc32(data));
    }
}
