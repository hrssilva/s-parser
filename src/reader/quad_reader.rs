use super::Reader;
use crate::{
    stream::{SeekableStream, Stream},
    tokenizer::{DecodeError, LineTokenizer, TermType, Token, Tokenizer},
};
use std::{fmt, io, ops::Range};

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

#[derive(Clone, Copy, Debug)]
pub struct Term<'a> {
    token: Token,
    record: &'a [u8],
}
impl<'a> Term<'a> {
    pub fn kind(self) -> TermType {
        self.token.kind()
    }
    pub fn text(self) -> &'a [u8] {
        self.token.text(self.record).unwrap()
    }
    /// Serialized lexical value; escape sequences have not been unescaped.
    pub fn value(self) -> &'a [u8] {
        self.token.value(self.record).unwrap()
    }
    pub fn suffix(self) -> &'a [u8] {
        self.token.suffix(self.record).unwrap()
    }
}

#[derive(Debug)]
pub struct Quad<'a> {
    record: &'a [u8],
    tokens: [Token; 4],
    offset: u64,
}
impl<'a> Quad<'a> {
    pub(super) fn with_base(mut self, base: u64) -> Self {
        self.offset += base;
        self
    }
    pub fn bytes(&self) -> &'a [u8] {
        self.record
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn tokens(&self) -> &[Token; 4] {
        &self.tokens
    }
    pub fn subject(&self) -> Term<'a> {
        self.term(0)
    }
    pub fn predicate(&self) -> Term<'a> {
        self.term(1)
    }
    pub fn object(&self) -> Term<'a> {
        self.term(2)
    }
    pub fn graph(&self) -> Option<Term<'a>> {
        (self.tokens[3].kind() != TermType::DefaultGraph).then(|| self.term(3))
    }
    fn term(&self, index: usize) -> Term<'a> {
        Term {
            token: self.tokens[index],
            record: self.record,
        }
    }
}

/// Owns its source window and decoder; no shared cursor or per-record allocation.
/// Errors are terminal: subsequent calls return `Ok(None)`.
///
/// ```compile_fail
/// use s_parser::{QuadReader, SliceStream, NtTokenizer};
/// let mut reader = QuadReader::new(SliceStream::new(b"<s:a><p:b><o:c>.\n", 64).unwrap(), NtTokenizer);
/// let quad = reader.next().unwrap().unwrap();
/// reader.next().unwrap(); // Cannot release bytes still borrowed by quad.
/// println!("{:?}", quad.subject().text());
/// ```
pub struct QuadReader<S, T> {
    stream: S,
    tokenizer: T,
    position: u64,
    end: u64,
    done: bool,
}

struct Prepared {
    start: usize,
    len: usize,
    tokens: [Token; 4],
    offset: u64,
}
impl<S: Stream, T: Tokenizer<Output = [Token; 4]>> QuadReader<S, T> {
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
    /// Returns a borrowed quad, EOF, or an error with an absolute byte offset.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Quad<'_>>, ParseError> {
        if self.done {
            return Ok(None);
        }
        let prepared = self.prepare();
        match prepared {
            Ok(Some(record)) => Ok(Some(Quad {
                record: &self.stream.bytes()[record.start..record.start + record.len],
                tokens: record.tokens,
                offset: record.offset,
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
    fn prepare(&mut self) -> Result<Option<Prepared>, ParseError> {
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
                            .is_some_and(|ts| ts.iter().any(|t| t.end() > decoded.consumed))
                    {
                        return Err(ParseError::DecoderContract {
                            offset: self.position,
                        });
                    }
                    let offset = self.position;
                    self.position += decoded.consumed as u64;
                    if let Some(tokens) = decoded.tokens {
                        return Ok(Some(Prepared {
                            start,
                            len: decoded.consumed,
                            tokens,
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

impl<S: SeekableStream, T: LineTokenizer<Output = [Token; 4]>> QuadReader<S, T> {
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
impl<S: Stream, T: Tokenizer<Output = [Token; 4]>> Reader for QuadReader<S, T> {
    type Record<'a>
        = Quad<'a>
    where
        Self: 'a;
    type Error = ParseError;
    fn next(&mut self) -> Result<Option<Quad<'_>>, ParseError> {
        QuadReader::next(self)
    }
}
