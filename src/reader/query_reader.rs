//! SPARQL's reader: a name for [`super::RecordReader`]/[`super::Record`]
//! specialized to the [`QueryData`] shape [`crate::sparql::SparqlTokenizer`]
//! produces, plus [`parse_query`], the common one-query-in-memory entry
//! point. There is no separate reading mechanism here — see
//! [`super::RecordReader`]'s doc comment.
//!
//! A SPARQL query is self-delimiting by its own grammar (`Prologue`, a query
//! form, solution modifiers, then an optional `ValuesClause` — nothing more
//! belongs to that `QueryUnit`), the same way an N-Triples record is
//! self-delimited by `.` and a line terminator. `SparqlTokenizer` treats one
//! `QueryUnit` as one record, so `QueryReader` can walk a *file of many
//! concatenated queries* — a query-log or benchmark corpus — with the same
//! bounded-window `Stream` machinery `QuadReader` uses for data files,
//! `FileStream`/`GzStream` included.
//!
//! Unlike `QuadReader`, there is no `for_range`/parallel support: SPARQL
//! property paths and long string literals can contain raw CR/LF, so this
//! tokenizer does not implement `LineTokenizer` and arbitrary byte-range
//! partitioning would be unsafe.
//!
//! A `QueryData` here plays the same role `[Token; 4]` plays for N-Triples —
//! the tokenizer's compact per-record output, built once and dropped with
//! the `Query` that owns it, same as `Quad`/`Token`. The one structural
//! difference is size: an N-Triples record is always exactly 4 terms, so
//! `[Token; 4]` is a fixed-size, stack-resident value with nothing to
//! allocate; a SPARQL algebra tree has no such bound, so its pools (see
//! `sparql::algebra::QueryData`) are unavoidably heap-allocated. Every span
//! *within* that tree — every `Term`, every `Iri` — still only ever holds
//! `u32` offsets into the query's own bytes, the same borrowing discipline
//! `rdf::Token` uses; nothing here copies query text.
use super::{ParseError, Record, RecordReader};
use crate::rdf::ResolvedIri;
use crate::sparql::SparqlTokenizer;
use crate::sparql::algebra::QueryData;
use crate::sparql::term::Iri;
use crate::tokenizer::{DecodeError, Tokenizer};

pub type QueryReader<S, T = SparqlTokenizer> = RecordReader<S, T>;
pub type Query<'a> = Record<'a, QueryData>;

impl<'a> Query<'a> {
    /// Resolve a prefixed name against this query's `PREFIX` declarations
    /// without allocating; see `Prologue::resolve`.
    pub fn resolve(&self, iri: Iri) -> Option<ResolvedIri<'a>> {
        self.data().prologue.resolve(iri, self.bytes())
    }
}

/// Parse one query already fully in memory — the common case. Bypasses
/// `Stream`/`QueryReader` entirely (there is nothing to window: the input is
/// already one contiguous, caller-owned slice), which is also why this can
/// return a `Query<'_>` borrowing `src` directly rather than through a
/// reader — a `Query` normally borrows the `QueryReader` that produced it,
/// the same lending contract `QuadReader`/`Quad` follow. Errors if anything
/// but trailing whitespace/comments follows the one `QueryUnit`.
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
            Ok(Record::new(&src[..consumed], data, 0))
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
