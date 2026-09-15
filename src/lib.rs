//! Bounded, borrowing parsers: sources own bytes, readers walk them, and
//! tokenizers describe records with compact offsets. See the README for usage.
#![doc = include_str!("../README.md")]
pub mod rdf;
pub mod reader;
pub mod sparql;
pub mod stream;
pub mod tokenizer;

pub use rdf::{Term, TermType, Token};
pub use reader::ParseError;
pub use reader::parallel::{ParallelReadStats, read_gzip_parallel};
pub use reader::quad_reader::{Quad, QuadReader};
pub use reader::query_reader::{Query, QueryReader, parse_query};
pub use sparql::SparqlTokenizer;
pub use stream::file_stream::FileStream;
pub use stream::gz_stream::GzStream;
pub use stream::{SeekableStream, SliceStream, Stream};
pub use tokenizer::nt_tokenizer::NtTokenizer;
pub use tokenizer::{DecodeError, Decoded, LineTokenizer, Tokenizer};
