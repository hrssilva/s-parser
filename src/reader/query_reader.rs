//! Reads SPARQL query text the same way [`super::quad_reader`] reads RDF
//! data: a `Stream` supplies bounded windows, [`crate::sparql::SparqlTokenizer`]
//! decodes one self-delimited `QueryUnit` at a time — see the `sparql`
//! module's doc comment for why that boundary exists — and this reader lends
//! a [`Query`] borrowing the reader itself, exactly like `QuadReader` lends
//! `Quad`. That makes a *file of many concatenated queries* (a query-log or
//! benchmark corpus) readable with the same bounded-memory `FileStream`/
//! `GzStream` machinery data files use, for the cost of this one file.
//!
//! Unlike `QuadReader`, there is no `for_range`/parallel support: SPARQL
//! property paths and long string literals can contain raw CR/LF, so this
//! tokenizer does not implement `LineTokenizer` and arbitrary byte-range
//! partitioning would be unsafe.
use super::{ParseError, Reader};
use crate::sparql::SparqlTokenizer;
use crate::sparql::algebra::QueryData;
use crate::sparql::term::{Iri, ResolvedIri};
use crate::stream::Stream;
use crate::tokenizer::{DecodeError, Tokenizer};

/// One parsed `QueryUnit`: the arena [`QueryData`] (all spans as `u32`
/// offsets — see `sparql::term::Span`) paired with the exact source bytes
/// those offsets index into, the same pairing [`super::quad_reader::Quad`]
/// gives N-Triples tokens.
#[derive(Debug)]
pub struct Query<'a> {
    data: QueryData,
    src: &'a [u8],
    offset: u64,
}
impl<'a> Query<'a> {
    pub fn bytes(&self) -> &'a [u8] {
        self.src
    }
    /// Absolute offset, in decoded bytes, of this query's first byte.
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn data(&self) -> &QueryData {
        &self.data
    }
    /// Resolve a prefixed name against this query's `PREFIX` declarations
    /// without allocating; see [`Prologue::resolve`].
    pub fn resolve(&self, iri: Iri) -> Option<ResolvedIri<'a>> {
        self.data.prologue.resolve(iri, self.src)
    }
}

struct Prepared {
    start: usize,
    len: usize,
    data: QueryData,
    offset: u64,
}

/// Owns its source window and decoder; no shared cursor or per-record
/// allocation beyond each query's own arena. Errors are terminal:
/// subsequent calls return `Ok(None)` — see [`super::quad_reader::QuadReader`],
/// whose contract this mirrors exactly.
pub struct QueryReader<S, T = SparqlTokenizer> {
    stream: S,
    tokenizer: T,
    position: u64,
    done: bool,
}
impl<S: Stream, T: Tokenizer<Output = QueryData>> QueryReader<S, T> {
    pub fn new(stream: S, tokenizer: T) -> Self {
        Self {
            stream,
            tokenizer,
            position: 0,
            done: false,
        }
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    /// Returns a borrowed query, EOF, or an error with an absolute byte offset.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Query<'_>>, ParseError> {
        if self.done {
            return Ok(None);
        }
        match self.prepare() {
            Ok(Some(p)) => Ok(Some(Query {
                src: &self.stream.bytes()[p.start..p.start + p.len],
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
    fn prepare(&mut self) -> Result<Option<Prepared>, ParseError> {
        loop {
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
                    if decoded.consumed == 0 || decoded.consumed > bytes.len() {
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
    }
}
impl<S: Stream, T: Tokenizer<Output = QueryData>> Reader for QueryReader<S, T> {
    type Record<'a>
        = Query<'a>
    where
        Self: 'a;
    type Error = ParseError;
    fn next(&mut self) -> Result<Option<Query<'_>>, ParseError> {
        QueryReader::next(self)
    }
}

/// Parse one query already fully in memory — the common case. Bypasses
/// `Stream`/`QueryReader` entirely (there is nothing to window: the input is
/// already one contiguous, caller-owned slice), which is also why this can
/// return a `Query<'_>` borrowing `src` directly rather than a reader — see
/// the `QueryReader::next` lending contract this sidesteps. Errors if
/// anything but trailing whitespace/comments follows the one `QueryUnit`.
pub fn parse_query(src: &[u8]) -> Result<Query<'_>, ParseError> {
    match SparqlTokenizer.decode(src, true) {
        Ok(crate::tokenizer::Decoded {
            tokens: Some(data),
            consumed,
        }) => {
            let rest = &src[consumed..];
            if !rest.is_empty() {
                let trailing_only_trivia =
                    matches!(SparqlTokenizer.decode(rest, true), Ok(d) if d.tokens.is_none());
                if !trailing_only_trivia {
                    return Err(ParseError::Syntax {
                        offset: consumed as u64,
                        message: "unexpected content after query",
                    });
                }
            }
            Ok(Query {
                data,
                src: &src[..consumed],
                offset: 0,
            })
        }
        Ok(crate::tokenizer::Decoded { tokens: None, .. }) => Err(ParseError::Syntax {
            offset: 0,
            message: "empty query",
        }),
        Err(DecodeError::NeedMore) => Err(ParseError::Syntax {
            offset: src.len() as u64,
            message: "unexpected end of input",
        }),
        Err(DecodeError::Invalid { offset, message }) => Err(ParseError::Syntax {
            offset: offset as u64,
            message,
        }),
    }
}
