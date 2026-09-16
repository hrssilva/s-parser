pub mod parallel;
pub mod quad_reader;
pub mod query_reader;
pub mod turtle_reader;

use crate::stream::{SeekableStream, Stream};
use crate::tokenizer::{DecodeError, LineTokenizer, Tokenizer};
use std::{fmt, io, ops::Range};

/// A lending reader: returned records borrow this reader, so advancing cannot
/// invalidate live token text. Ordinary Iterator cannot express this lifetime.
pub trait Reader {
    type Record<'a>
    where
        Self: 'a;
    type Error;
    fn next(&mut self) -> Result<Option<Self::Record<'_>>, Self::Error>;
}

/// One decoded record: a `Tokenizer`'s `Output` paired with the exact source
/// bytes it was decoded from, and that record's absolute offset. Format
/// specifics (`.subject()`, `.resolve()`, ...) live as inherent impls on
/// `Record<'a, D>` for the concrete `D` a given format's tokenizer produces
/// — see `QuadReader`'s `Quad<'a> = Record<'a, [Token; 4]>` and
/// `QueryReader`'s `Query<'a> = Record<'a, QueryData>` for the pattern.
#[derive(Debug)]
pub struct Record<'a, D> {
    bytes: &'a [u8],
    data: D,
    offset: u64,
}
impl<'a, D> Record<'a, D> {
    pub(crate) fn new(bytes: &'a [u8], data: D, offset: u64) -> Self {
        Self {
            bytes,
            data,
            offset,
        }
    }
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn data(&self) -> &D {
        &self.data
    }
    pub(crate) fn with_base(mut self, base: u64) -> Self {
        self.offset += base;
        self
    }
}

struct Prepared<D> {
    start: usize,
    len: usize,
    data: D,
    offset: u64,
}

/// The reading mechanism shared by every format this crate parses: walk a
/// `Stream`'s windows, decode one record at a time via a `Tokenizer`, lend
/// it borrowing this reader. This is the *entire* mechanism — window
/// loading, `NeedMore` retry, decoder/source contract checks, comment/blank
/// record skipping — independent of what a record decodes into. `QuadReader`
/// (N-Triples) and `QueryReader` (SPARQL) are both this type under a
/// format-specific name (`pub type ... = RecordReader<S, T>;`); a future
/// format's reader is a third alias, not a new implementation of this loop.
///
/// Owns its source window and decoder; no shared cursor or per-record
/// allocation beyond what the tokenizer's own `Output` needs. Errors are
/// terminal: subsequent calls return `Ok(None)`.
pub struct RecordReader<S, T> {
    stream: S,
    tokenizer: T,
    position: u64,
    end: u64,
    done: bool,
}
impl<S: Stream, T: Tokenizer> RecordReader<S, T> {
    pub fn new(stream: S, tokenizer: T) -> Self {
        Self {
            stream,
            tokenizer,
            position: 0,
            end: u64::MAX,
            done: false,
        }
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    /// Returns a borrowed record, EOF, or an error with an absolute byte offset.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Record<'_, T::Output>>, ParseError> {
        if self.done {
            return Ok(None);
        }
        match self.prepare() {
            Ok(Some(p)) => Ok(Some(Record {
                bytes: &self.stream.bytes()[p.start..p.start + p.len],
                data: p.data,
                offset: p.offset,
            })),
            Ok(None) => {
                self.done = true;
                Ok(None)
            }
            Err(error) => {
                self.done = true;
                Err(error)
            }
        }
    }
    fn prepare(&mut self) -> Result<Option<Prepared<T::Output>>, ParseError> {
        while self.position < self.end {
            if self.position < self.stream.offset()
                || self.position - self.stream.offset() >= self.stream.bytes().len() as u64
            {
                if self.position >= self.stream.offset() && self.stream.is_eof() {
                    return Ok(None);
                }
                self.stream.load(self.position)?;
            }
            let start = (self.position - self.stream.offset()) as usize;
            let bytes = self
                .stream
                .bytes()
                .get(start..)
                .ok_or(ParseError::DecoderContract {
                    offset: self.position,
                })?;
            if bytes.is_empty() {
                if self.stream.is_eof() {
                    return Ok(None);
                }
                return Err(ParseError::DecoderContract {
                    offset: self.position,
                });
            }
            let eof = self.stream.is_eof();
            match self.tokenizer.decode(bytes, eof) {
                Ok(decoded) => {
                    if decoded.consumed == 0
                        || decoded.consumed > bytes.len()
                        || decoded
                            .tokens
                            .as_ref()
                            .is_some_and(|d| !self.tokenizer.bounds_ok(d, decoded.consumed))
                    {
                        return Err(ParseError::DecoderContract {
                            offset: self.position,
                        });
                    }
                    let offset = self.position;
                    self.position += decoded.consumed as u64;
                    if let Some(data) = decoded.tokens {
                        return Ok(Some(Prepared {
                            start,
                            len: decoded.consumed,
                            data,
                            offset,
                        }));
                    }
                }
                Err(DecodeError::NeedMore) => {
                    if eof {
                        return Err(ParseError::Syntax {
                            offset: self.position + bytes.len() as u64,
                            message: "unexpected end of input",
                        });
                    }
                    if start == 0 {
                        return Err(ParseError::RecordTooLong {
                            offset: self.position,
                            limit: self.stream.window_size(),
                        });
                    }
                    self.stream.load(self.position)?;
                }
                Err(DecodeError::Invalid { offset, message }) => {
                    return Err(ParseError::Syntax {
                        offset: self.position.saturating_add(offset as u64),
                        message,
                    });
                }
            }
        }
        Ok(None)
    }
}
impl<S: SeekableStream, T: LineTokenizer> RecordReader<S, T> {
    /// Own records whose physical line starts in `range`. Adjacent ranges cover
    /// each record exactly once, even when a boundary cuts a term or CRLF.
    /// Partitioning is only available for explicitly line-delimited decoders.
    pub fn for_range(mut stream: S, tokenizer: T, range: Range<u64>) -> Result<Self, ParseError> {
        if range.start > range.end || range.end > stream.len() {
            return Err(ParseError::InvalidRange);
        }
        let mut position = range.start;
        if position != 0 && position < range.end {
            // Inspect the previous byte to distinguish a line start from its tail.
            stream.load(position - 1)?;
            if !matches!(stream.bytes().first(), Some(b'\r' | b'\n')) {
                while position < range.end {
                    if position - stream.offset() >= stream.bytes().len() as u64 {
                        stream.load(position)?;
                    }
                    let start = (position - stream.offset()) as usize;
                    let bytes = &stream.bytes()[start..];
                    if bytes.is_empty() {
                        return Err(ParseError::DecoderContract { offset: position });
                    }
                    let available = bytes
                        .len()
                        .min((range.end - position).min(usize::MAX as u64) as usize);
                    if let Some(end) = bytes[..available]
                        .iter()
                        .position(|b| matches!(b, b'\r' | b'\n'))
                    {
                        position += end as u64 + 1;
                        break;
                    }
                    position += available as u64;
                }
            }
        }
        Ok(Self {
            stream,
            tokenizer,
            position,
            end: range.end,
            done: false,
        })
    }
}
impl<S: Stream, T: Tokenizer> Reader for RecordReader<S, T> {
    type Record<'a>
        = Record<'a, T::Output>
    where
        Self: 'a;
    type Error = ParseError;
    fn next(&mut self) -> Result<Option<Record<'_, T::Output>>, ParseError> {
        RecordReader::next(self)
    }
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
