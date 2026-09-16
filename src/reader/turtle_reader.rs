//! Turtle's reader: a name for [`super::RecordReader`]/[`super::Record`]
//! specialized to the [`StatementData`] shape [`crate::turtle::TurtleTokenizer`]
//! produces. There is no separate reading mechanism here — see
//! [`super::RecordReader`]'s doc comment.
//!
//! One record here is one Turtle statement (directive, or `;`/`,`-expanded
//! triples), the same granularity `crate::turtle`'s module doc comment
//! describes. `StatementData` plays the same role `[Token; 4]` plays for
//! N-Triples and `QueryData` plays for one SPARQL query: the tokenizer's
//! compact per-record output, built once and dropped with the `Statement`
//! that owns it. Unlike either of those, it's a fixed-size (not growable)
//! stack array — see `turtle::term::MAX_TRIPLES_PER_STATEMENT` — since a
//! statement, unlike a whole query, has one natural per-item cost (one
//! triple) to bound. Every span it holds is still only ever a `u32` offset
//! into this statement's own bytes, except a resolved prefixed name's
//! namespace, which is a small inline copy — see `turtle::term::Namespace`
//! for why that one piece can't stay a same-statement span.
use super::{Record, RecordReader};
use crate::turtle::TurtleTokenizer;
use crate::turtle::term::{StatementData, TriplePattern};

pub type TurtleReader<S, T = TurtleTokenizer> = RecordReader<S, T>;
pub type Statement<'a> = Record<'a, StatementData>;

impl<'a> Statement<'a> {
    /// This statement's triples. Empty only if a directive produced this
    /// record — which [`TurtleTokenizer`] never does; a directive reports
    /// `tokens: None` instead, so `TurtleReader::next` skips it the same way
    /// it skips a comment or blank input, and a caller never sees an empty
    /// `Statement`.
    pub fn triples(&self) -> &[TriplePattern] {
        self.data().triples()
    }
}
