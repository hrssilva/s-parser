//! Turtle: a hand-written recursive-descent parser producing one
//! [`term::StatementData`] per statement (a directive, or a triples
//! statement) — the same "compact record metadata" role
//! [`crate::tokenizer::nt_tokenizer`] plays for N-Triples and
//! [`crate::sparql`] plays for SPARQL, just at the granularity of one
//! `;`/`,`-expanded statement instead of one triple or one whole query. See
//! [`crate::rdf`] for why `turtle::Term` and `rdf::Term` are deliberately
//! different types.
//!
//! Turtle differs from both of those in one structural way: `@prefix` is
//! scoped to the rest of the *document*, not to one record, and blank-node
//! identity (`[]`, `_:b`, list/property-list expansion) must stay distinct
//! across the whole document too. Neither can live in the per-statement
//! [`term::StatementData`] the way a SPARQL query's `PREFIX`s live in that
//! one query's own `QueryData` — they have to outlive it. So
//! [`TurtleTokenizer`] itself carries that state (a small prefix-name table
//! and a blank-node counter) across every `decode` call for as long as the
//! tokenizer lives, the same way a hand-rolled streaming parser would, while
//! still handing back a `StatementData` per statement that allocates nothing
//! for the common case (see that type's doc comment): resolving a prefixed
//! name copies its (short, `@prefix`-bounded) namespace IRI into the term
//! inline (see [`term::Namespace`]) rather than borrowing the table or the
//! declaring statement's own (possibly long-gone) window.
//!
//! A Turtle statement is self-delimiting by its own grammar (ends at the
//! `.` terminating a `directive` or `triples`), the same way an N-Triples
//! record is. `TurtleTokenizer` treats one statement as one record, so
//! [`crate::reader::turtle_reader::TurtleReader`] walks a Turtle document —
//! `FileStream`/`GzStream` included — with the same bounded-window `Stream`
//! machinery every other reader in this crate uses. Turtle's long string
//! literals and IRIREFs can contain raw CR/LF, so — like SPARQL, unlike
//! N-Triples — this tokenizer does not implement `LineTokenizer`.
//!
//! One statement can expand (via `;`, `,`, blank-node property lists, and
//! collections) into many triples; [`term::StatementData`] holds a small
//! number of them inline with no allocation and spills the rest into a
//! `Vec` only when a statement genuinely needs it — see that type's doc
//! comment for why this isn't a fixed, error-on-overflow bound the way
//! `window_size`/`RecordTooLong` is. Consistent with the rest of this crate,
//! this stays a lexical/structural layer: relative-IRI resolution against
//! `BASE` and `PN_LOCAL` escape removal are left to the caller.
mod lexer;
mod parser;
pub mod term;

use crate::tokenizer::{DecodeError, Decoded, Tokenizer};
use term::StatementData;

/// Decodes one Turtle statement per [`Tokenizer::decode`] call, carrying
/// `@prefix` declarations and blank-node identity across the whole document
/// — see this module's doc comment. Used directly by
/// [`crate::reader::turtle_reader::TurtleReader`].
#[derive(Clone, Debug, Default)]
pub struct TurtleTokenizer {
    prefixes: Vec<(Vec<u8>, Vec<u8>)>,
    blank_counter: u32,
}

impl Tokenizer for TurtleTokenizer {
    type Output = StatementData;
    fn decode(&mut self, bytes: &[u8], eof: bool) -> Result<Decoded<StatementData>, DecodeError> {
        if bytes.len() > u32::MAX as usize {
            return Err(DecodeError::Invalid {
                offset: 0,
                message: "statement exceeds 32-bit token offsets",
            });
        }
        let mut probe = lexer::Scanner::new(bytes, eof);
        probe.skip_trivia();
        match probe.peek() {
            None if eof => Ok(Decoded {
                tokens: None,
                consumed: probe.pos,
            }),
            None => Err(DecodeError::NeedMore),
            Some(_) => {
                let (tokens, consumed) =
                    parser::Parser::new(bytes, eof, &mut self.prefixes, &mut self.blank_counter)
                        .parse()?;
                Ok(Decoded { tokens, consumed })
            }
        }
    }
    /// `StatementData`'s spans, and every `Namespace` it copies bytes into,
    /// are produced during the same scan that produces `consumed` (spans are
    /// recorded offsets, never freely constructed; `Namespace::new` copies
    /// bytes, not a span, so it holds no offset into this record at all), so
    /// an out-of-bounds span is impossible by construction — see
    /// `sparql::SparqlTokenizer` for the same reasoning.
    fn bounds_ok(&self, _output: &StatementData, _consumed: usize) -> bool {
        true
    }
}
