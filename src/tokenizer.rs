//! Token offsets are relative to one record, never to the entire file. This
//! module is the parsing *mechanism* — record decoding, independent of any
//! one data model. Concrete RDF term types (`Token`, `TermType`) live in
//! [`crate::rdf`]; `Decoded`'s default `Output` references them only as a
//! convenience for RDF-shaped tokenizers like [`nt_tokenizer`].
pub mod nt_tokenizer;

use crate::rdf::Token;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    NeedMore,
    Invalid {
        offset: usize,
        message: &'static str,
    },
}

/// `None` tokens denote a consumed comment or blank line. `consumed` must be
/// positive and includes the line terminator (if present).
#[derive(Clone, Copy, Debug)]
pub struct Decoded<R = [Token; 4]> {
    pub tokens: Option<R>,
    pub consumed: usize,
}

pub trait Tokenizer {
    /// Compact metadata produced for a record; the reader interprets its shape.
    type Output;
    /// Decode one record. Return NeedMore only if a larger window could help;
    /// `eof` says this slice ends at the physical end of the source. NeedMore
    /// must leave record state unchanged: the reader retries the same prefix.
    fn decode(&mut self, bytes: &[u8], eof: bool) -> Result<Decoded<Self::Output>, DecodeError>;
}

/// Opt-in for formats whose records cannot contain raw CR or LF. This enables
/// independent readers to align arbitrary byte partitions without coordination.
pub trait LineTokenizer: Tokenizer {}
