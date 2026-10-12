//! Concrete reader-to-writer conversion loops — the actual "convert this
//! file from one RDF syntax to another" entry points, built on
//! [`crate::rdf::ResolvedTerm`] as the shared currency between any reader
//! format's own `Term` type and any [`crate::writer`]. Each function here
//! is concrete (one source format, one sink type), the same "concrete
//! shape first" precedent the rest of this crate follows: a generic
//! `Reader -> Writer` driver can follow once a second concrete writer shows
//! what that abstraction should look like.
//!
//! Every one of these streams through the source `Stream`'s own bounded
//! window rather than requiring the whole input in memory, so they work
//! the same way over an in-memory slice, a memory-mapped file, or gzip
//! input as every other reader in this crate does.
use crate::rdf::Token;
use crate::reader::ParseError;
use crate::reader::quad_reader::QuadReader;
use crate::reader::turtle_reader::TurtleReader;
use crate::stream::Stream;
use crate::tokenizer::Tokenizer;
use crate::writer::nt_writer::NtWriter;
use std::{fmt, io};

/// Either half of a conversion can fail: reading (syntax, I/O, a window too
/// small for one record) or writing (the sink's own I/O). Kept as two
/// variants, rather than reusing `ParseError` for both, so a caller can
/// always tell which side failed.
#[derive(Debug)]
pub enum ConvertError {
    Read(ParseError),
    Write(io::Error),
}
impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "read error: {e}"),
            Self::Write(e) => write!(f, "write error: {e}"),
        }
    }
}
impl std::error::Error for ConvertError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            Self::Write(e) => Some(e),
        }
    }
}
impl From<ParseError> for ConvertError {
    fn from(e: ParseError) -> Self {
        Self::Read(e)
    }
}
impl From<io::Error> for ConvertError {
    fn from(e: io::Error) -> Self {
        Self::Write(e)
    }
}

/// Streams every triple from a Turtle document into N-Triples-shaped
/// output (also valid Turtle — see `writer::nt_writer`'s doc comment).
pub fn turtle_to_ntriples<S: Stream>(
    reader: &mut TurtleReader<S>,
    out: &mut NtWriter<impl io::Write>,
) -> Result<(), ConvertError> {
    while let Some(stmt) = reader.next()? {
        let record = stmt.bytes();
        for triple in stmt.triples() {
            out.write_statement(
                triple.subject.resolve(record),
                triple.predicate.resolve(record),
                triple.object.resolve(record),
                None,
            )?;
        }
    }
    Ok(())
}

/// Streams every quad from an N-Triples/N-Quads source into canonical
/// N-Triples output — a round-trip/canonicalization pass proving the same
/// `resolve`-then-write shape `turtle_to_ntriples` uses works for N-Triples'
/// own, already-resolved terms too.
pub fn ntriples_to_ntriples<S: Stream, T: Tokenizer<Output = [Token; 4]>>(
    reader: &mut QuadReader<S, T>,
    out: &mut NtWriter<impl io::Write>,
) -> Result<(), ConvertError> {
    while let Some(quad) = reader.next()? {
        out.write_statement(
            quad.subject().resolve(),
            quad.predicate().resolve(),
            quad.object().resolve(),
            quad.graph().map(|g| g.resolve()),
        )?;
    }
    Ok(())
}
