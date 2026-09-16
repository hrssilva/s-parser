//! SPARQL 1.1 Query: a hand-written recursive-descent parser that translates
//! directly into a compact SPARQL algebra tree (see [`algebra`]) instead of
//! an intermediate concrete-syntax AST — the same "compact record metadata"
//! role [`crate::tokenizer::nt_tokenizer`] plays for N-Triples, just
//! variable-size instead of a fixed `[Token; 4]`. See [`crate::rdf`] for why
//! `sparql::Term` and `rdf::Term` are deliberately different types.
//!
//! A SPARQL query is self-delimiting by its own grammar (`Prologue`, a query
//! form, solution modifiers, then an optional `ValuesClause` — nothing more
//! belongs to that `QueryUnit`), the same way an N-Triples record is
//! self-delimited by `.` and a line terminator. [`SparqlTokenizer`] treats
//! one `QueryUnit` as one record, so [`crate::reader::query_reader::QueryReader`]
//! can walk a *file of many concatenated queries* — a query-log or benchmark
//! corpus — with the same bounded-window `Stream` machinery `QuadReader`
//! uses for data files, `FileStream`/`GzStream` included, at essentially no
//! extra cost. Property paths and Turtle-style long string literals can
//! both legally contain raw CR/LF, so unlike `NtTokenizer` this tokenizer
//! does not implement `LineTokenizer`: arbitrary byte-range partitioning
//! (`QuadReader::for_range`, `read_gzip_parallel`) is not offered here.
//!
//! Scope: SELECT/ASK/CONSTRUCT/DESCRIBE, basic graph patterns, FILTER/
//! OPTIONAL/UNION/MINUS/BIND/VALUES/GRAPH, subqueries, property paths, the
//! full builtin function and aggregate vocabulary, GROUP BY/HAVING/ORDER BY/
//! LIMIT/OFFSET. `SERVICE` (federated query) is not supported. SPARQL
//! Update is not implemented. Consistent with the rest of this crate, this
//! stays a lexical/structural layer: relative-IRI resolution against `BASE`,
//! `PN_LOCAL` escape removal, and grammar-level arity/scope validation
//! beyond what parsing itself requires are left to the caller — see the
//! doc comments on [`term::Prologue::resolve`] and [`expr::Expr::Aggregate`].
pub mod algebra;
pub mod expr;
pub mod ids;
mod lexer;
mod parser;
pub mod path;
pub mod pool;
pub mod term;

use crate::tokenizer::{DecodeError, Decoded, Tokenizer};
use algebra::QueryData;

/// Decodes one `QueryUnit` per [`Tokenizer::decode`] call. Used directly by
/// [`crate::reader::query_reader::QueryReader`] and
/// [`crate::reader::query_reader::parse_query`]; most callers want the
/// latter.
#[derive(Clone, Copy, Debug, Default)]
pub struct SparqlTokenizer;

impl Tokenizer for SparqlTokenizer {
    type Output = QueryData;
    fn decode(&mut self, bytes: &[u8], eof: bool) -> Result<Decoded<QueryData>, DecodeError> {
        if bytes.len() > u32::MAX as usize {
            return Err(DecodeError::Invalid {
                offset: 0,
                message: "query exceeds 32-bit token offsets",
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
                let (data, consumed) = parser::Parser::new(bytes, eof).parse_query_unit()?;
                Ok(Decoded {
                    tokens: Some(data),
                    consumed,
                })
            }
        }
    }
    /// `QueryData`'s spans are all `u32` offsets recorded by the scanner
    /// during the same pass that produces `consumed` (`Span`s pushed into
    /// its pools, never freely constructed), so a span past `consumed` is
    /// impossible by construction — see [`term::Span`] and the arena
    /// pools on [`algebra::QueryData`].
    fn bounds_ok(&self, _output: &QueryData, _consumed: usize) -> bool {
        true
    }
}
