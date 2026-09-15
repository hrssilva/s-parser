pub mod parallel;
pub mod quad_reader;
pub mod query_reader;

use std::{fmt, io};

/// A lending reader: returned records borrow this reader, so advancing cannot
/// invalidate live token text. Ordinary Iterator cannot express this lifetime.
pub trait Reader {
    type Record<'a>
    where
        Self: 'a;
    type Error;
    fn next(&mut self) -> Result<Option<Self::Record<'_>>, Self::Error>;
}

/// Shared across every `Reader` in this crate: reader-level failure is about
/// record decoding mechanics (I/O, byte ranges, decoder/source contract),
/// never about a specific data model, so one error type serves `QuadReader`
/// and `QueryReader` alike.
#[derive(Debug)]
pub enum ParseError {
    Io(io::Error),
    InvalidRange,
    Syntax { offset: u64, message: &'static str },
    RecordTooLong { offset: u64, limit: usize },
    DecoderContract { offset: u64 },
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::InvalidRange => write!(f, "invalid reader byte range"),
            Self::Syntax { offset, message } => write!(f, "byte {offset}: {message}"),
            Self::RecordTooLong { offset, limit } => {
                write!(f, "record at byte {offset} exceeds {limit}-byte window")
            }
            Self::DecoderContract { offset } => {
                write!(f, "decoder/source contract violated at byte {offset}")
            }
        }
    }
}
impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}
impl From<io::Error> for ParseError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
