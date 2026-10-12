//! Serialization: the write-side counterpart to [`crate::reader`]. Writing
//! doesn't need the same `Stream`/`Tokenizer` windowing machinery reading
//! does — a resolved triple (see [`crate::rdf::ResolvedTerm`]) is always
//! fully in hand before a writer ever sees it, so there's no "not enough
//! bytes yet" to retry the way a `Stream` window can run short mid-record.
//! A writer is just something serializing already-resolved terms to an
//! `std::io::Write` sink, one statement at a time.
//!
//! Each concrete format gets its own writer type, the same "concrete
//! shape first" precedent [`crate::reader::quad_reader`]/
//! [`crate::reader::turtle_reader`] set for reading: a shared `Writer`
//! trait is deferred until a second concrete writer shows what it should
//! look like, rather than guessed at with only one data point.
pub mod nt_writer;
