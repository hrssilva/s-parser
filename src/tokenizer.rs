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
    /// Defense-in-depth hook for the reader: does `output`, from a call that
    /// reported `consumed` bytes, claim nothing beyond them? The reader's
    /// windowing/retry loop is generic over `Output` and can't inspect its
    /// shape itself, so it can't independently guard against a *buggy*
    /// `Tokenizer` handing out a span past `consumed` (which would later
    /// panic wherever that span is read against the record's bytes).
    ///
    /// The default is `false`, not a permissive `true`: every `Tokenizer`
    /// must explicitly decide this rather than silently inheriting "trust
    /// me" — an implementor who never thinks about it gets every record
    /// rejected as a `DecoderContract` violation, a loud, immediate failure
    /// in testing, instead of a latent panic risk shipped by default.
    /// Implementations whose `Output` is built entirely from offsets
    /// recorded during the same scan that produced `consumed` (the common
    /// case) can simply return `true` unconditionally, since that
    /// construction already makes an out-of-bounds span impossible; see
    /// `sparql::SparqlTokenizer` for that case, and `nt_tokenizer::NtTokenizer`
    /// for one that inspects each span instead.
    fn bounds_ok(&self, output: &Self::Output, consumed: usize) -> bool {
        let _ = (output, consumed);
        false
    }
}

/// Opt-in for formats whose records cannot contain raw CR or LF. This enables
/// independent readers to align arbitrary byte partitions without coordination.
pub trait LineTokenizer: Tokenizer {}
