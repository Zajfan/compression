use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while decoding. Compression never fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Input ended before the decoder expected it to.
    UnexpectedEof,
    /// Input is malformed; the message says what was wrong.
    Corrupt(&'static str),
    /// Decoded size did not match the size recorded at compression time.
    LengthMismatch { expected: usize, actual: usize },
    /// Checksum of the decoded data did not match.
    ChecksumMismatch { expected: u32, actual: u32 },
    /// Not a frame produced by this program.
    BadMagic,
    /// Frame format version this build does not understand.
    UnsupportedVersion(u8),
    /// Frame names a codec this build does not know.
    UnknownCodec(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnexpectedEof => write!(f, "unexpected end of input"),
            Error::Corrupt(why) => write!(f, "corrupt data: {why}"),
            Error::LengthMismatch { expected, actual } => {
                write!(f, "expected {expected} bytes after decoding, got {actual}")
            }
            Error::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "checksum mismatch: expected {expected:08x}, got {actual:08x}"
                )
            }
            Error::BadMagic => write!(f, "not a cmpr file (bad magic bytes)"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported frame version {v}"),
            Error::UnknownCodec(id) => write!(f, "unknown codec id {id}"),
        }
    }
}

impl std::error::Error for Error {}
