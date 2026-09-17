# s-parser

A Rust parsing core organized around three components:

- `Stream` owns or borrows input bytes and exposes a bounded contiguous window.
- `Reader` lends records to the caller while walking a stream.
- `Tokenizer` produces compact record metadata without owning token text. Its
  associated `Output` type keeps the mechanism independent of RDF.

There is exactly one `Reader` implementation, `RecordReader<S, T>`: it owns
the entire windowing/retry mechanism — loading `Stream` windows, retrying on
`NeedMore`, translating decoder errors, skipping comments/blank records —
generically over whatever a `Tokenizer` decodes. It lends `Record<'a, D>`
(`D = T::Output`), pairing that decoded data with the exact source bytes it
came from. Every format-specific reader in this crate is `RecordReader` under
a `pub type` alias, not a separate implementation of that mechanism:
`QuadReader<S, T> = RecordReader<S, T>` for N-Triples (`NtTokenizer`, four RDF
term spans, the fourth representing the default graph) and `QueryReader<S, T>
= RecordReader<S, T>` for SPARQL (`SparqlTokenizer`) both reuse it as-is.
Format-specific ergonomics (`Quad::subject()`, `Query::resolve()`, ...) are
inherent methods on `Record<'a, D>` for the concrete `D` that format's
tokenizer produces — see `s_parser::reader`'s doc comment for the pattern. A
Turtle reader, when implemented, is a third alias, not a fourth mechanism.

`FileStream` supplies windowed file mappings; `SliceStream` borrows existing
memory. `GzStream` incrementally decompresses gzip files or any compressed
`Read` input. Turtle is not implemented yet.

SPARQL 1.1 Query is a second format built on the same mechanism: a SPARQL
query is self-delimiting by its own grammar, the same way an N-Triples record
is self-delimited by `.` and a line terminator, so `SparqlTokenizer` treats
one `QueryUnit` as one record and `QueryReader<S, T>` walks a *file of many
concatenated queries* with the same bounded-window `Stream` machinery —
`FileStream`/`GzStream` included. `parse_query` covers the common case of one
query already in memory. Query text parses directly into a compact SPARQL
algebra tree (`s_parser::sparql`) instead of an intermediate concrete-syntax
AST — see [SPARQL Query](#sparql-query) below.

## Add the library

For a local checkout, add a path dependency to your application's `Cargo.toml`:

```toml
[dependencies]
s-parser = { path = "../s-parser" }
```

The crate uses Rust edition 2024. Start with `SliceStream` for bytes already in
memory, `FileStream` for an immutable file, or `GzStream` for compressed input.
The reader and tokenizer are interchangeable across these sources.

## Read borrowed RDF terms

```rust
use s_parser::{NtTokenizer, QuadReader, SliceStream};

let input = b"<urn:subject> <urn:predicate> \"hello\"@en .\n";
let stream = SliceStream::new(input, 1024)?;
let mut reader = QuadReader::new(stream, NtTokenizer);
while let Some(quad) = reader.next()? {
    assert_eq!(quad.subject().value(), b"urn:subject");
    assert_eq!(quad.object().value(), b"hello");
    assert_eq!(quad.object().suffix(), b"@en");
    assert!(quad.graph().is_none());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

For files:

```rust,no_run
use s_parser::{FileStream, NtTokenizer, QuadReader};

// SAFETY: the input must remain unmodified and untruncated until every stream
// clone and reader is dropped, including modifications by other processes.
let stream = unsafe { FileStream::open("data.nt", 8 * 1024 * 1024)? };
let mut reader = QuadReader::new(stream, NtTokenizer);
while let Some(quad) = reader.next()? {
    // Consume borrowed text here, before advancing to the next record.
    std::hint::black_box(quad.object().value());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The unsafe constructor exposes the file-backed mmap immutability requirement.
A read-only file descriptor cannot enforce it. Use `SliceStream` when parsing
bytes whose lifetime and immutability are already managed by Rust.

For gzip files, the reader and tokenizer stay the same:

```rust,no_run
use s_parser::{GzStream, NtTokenizer, QuadReader};

let stream = GzStream::open("data.nt.gz", 8 * 1024 * 1024)?;
let mut reader = QuadReader::new(stream, NtTokenizer);
while let Some(quad) = reader.next()? {
    std::hint::black_box(quad.object().value());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`GzStream` uses [flate2](https://docs.rs/flate2/latest/flate2/) with the optimized
`zlib-rs` backend and runtime CPU detection. It reads compressed input through a
64 KiB buffer and inflates directly into the configured output window. It never
creates a decompressed file, scans ahead for a total length, or retains all input.
Opening the stream reads no data; the first reader call fills the working window.

## Memory and lifetimes

A `Token` is 16 bytes: a 32-bit kind and three 32-bit offsets for the start,
lexical end, and written end. Four tokens occupy 64 bytes. Absolute file
positions use `u64`; only offsets within one record use `u32`. `text()`,
`value()`, and `suffix()` borrow the source, with no per-term allocation.

Each file reader holds at most one mapping of the configured window size, plus
OS alignment overhead and fixed reader/decoder state. It unmaps the previous
window **before** mapping another. Parser-owned mapped memory therefore scales
with readers × window size, independently of file size. Thread stacks,
application allocations, and the OS's reclaimable file cache are separate from
that budget. A cloned `FileStream` shares an `Arc<File>` but retains no mapping
from the original.

When a record straddles a file mapping, the reader remaps at that record's start
and retries the decoder. For gzip, it moves the unfinished record's tail to the
start of the same output buffer, then inflates more bytes after it. Tokens borrow
that buffer directly. Gzip memory is one output window + 64 KiB input + fixed
inflater state + one lookahead byte, independent of the file's expanded size.
Optional gzip header names and comments are skipped without storing them.

The window size is also the maximum record size **after decompression**, including leading spaces,
trailing comments, and its line terminator. An oversized record or comment
returns `RecordTooLong`; it never grows a buffer. Window sizes must fit both
`u32` and `isize` and be positive. CR and LF each count as boundaries, so the LF
of CRLF is consumed as a blank line.

`next()` is a lending method, not `Iterator::next`: its result borrows the
reader. Rust prevents advancing or dropping the reader while borrowed term
bytes remain in use. Copy text explicitly if the application needs it longer.
Errors terminate a reader; subsequent calls return `Ok(None)`.

## Parallel file parsing

Clone the source once per worker and create readers with disjoint adjacent
byte ranges:

```rust,no_run
use s_parser::{FileStream, NtTokenizer, QuadReader, SeekableStream};
let source = unsafe { FileStream::open("data.nt", 8 * 1024 * 1024)? };
let midpoint = source.len() / 2;
let left = QuadReader::for_range(source.clone(), NtTokenizer, 0..midpoint)?;
let right = QuadReader::for_range(source.clone(), NtTokenizer, midpoint..source.len())?;
// Move left and right to independent workers.
# Ok::<(), Box<dyn std::error::Error>>(())
```

A reader owns records whose physical line starts in its range. It skips an
initial partial line and finishes a final record even when that extends beyond
its range. Adjacent ranges cover every record exactly once, including splits
inside UTF-8 characters, terms, comments, or CRLF. Empty ranges work. There is
no shared cursor, mutex, record queue, or allocation per record. Results within
a worker remain in source order; cross-worker ordering is the caller's choice.

Range alignment is available only for decoders implementing `LineTokenizer`
and sources implementing `SeekableStream` (`FileStream` and `SliceStream`).
A format with raw multiline records needs its own partitioning policy. New
sources must expose a full window (or the remaining input at EOF) from the
requested offset. `Stream::is_eof()` reports whether the current window reaches
EOF; a total length is only required by `SeekableStream::len()`. Forward-only
sources accept offsets inside their current window or at its end.
Stateful decoders must leave record state unchanged on `NeedMore`, because the
reader retries the same prefix after remapping.

Ordinary gzip requires sequential inflation: a random compressed offset lacks
the bit position and preceding dictionary needed to resume decoding. `GzStream`
itself is forward-only, without compressed range partitioning or cloning.
`read_gzip_parallel` overlaps this inflater with multiple readers of the decoded
bytes, using **two alternating shared windows**. The caller thread inflates one
window while persistent worker threads parse disjoint ranges of the other.
All workers borrow the same buffer; the buffer budget is **2 × window size**,
independent of worker count, plus fixed inflater/input state and worker overhead.
The inflater waits for every worker before reusing their buffer. Only the
unfinished trailing record is copied into the next buffer; whole windows and
token text are never copied for distribution.

```rust,no_run
use s_parser::{GzStream, NtTokenizer, read_gzip_parallel};

let stats = read_gzip_parallel(
    GzStream::open("data.nt.gz", 8 * 1024 * 1024)?,
    NtTokenizer,
    4, // parser workers, in addition to the caller's inflater
    |worker, quad| {
        // Borrowed text, absolute decoded offsets; cross-worker order is unspecified.
        std::hint::black_box((worker, quad.offset(), quad.object().value()));
    },
)?;
println!("{} triples, {} decoded bytes", stats.triples, stats.decoded_bytes);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The parallel function accepts cloneable `LineTokenizer` implementations that
produce quad spans. Consumer callbacks can run concurrently and must consume
borrowed values before returning; callbacks may already have processed records
when a later error occurs. Errors stop the pipeline and workers are joined before
returning. Callback panics propagate after worker shutdown. Reported inflation
and parsing times overlap and must not be added to estimate elapsed time.

Concatenated gzip members are decoded in order,
even when a member boundary splits a record. CRCs, trailer sizes, truncated input,
and trailing garbage are checked as the stream advances; consume it to EOF to
validate the entire archive. Offsets and record limits refer to decoded bytes.
`GzStream::decoded_len()` becomes available at EOF and supports files above 4 GiB.

The complete bounded counting example launches workers and retains only counts:

```sh
cargo run --release --example count -- data.nt 4 8
# file, thread count, window size in MiB
/usr/bin/time -v target/release/examples/count data.nt 4 8
target/release/examples/count data.nt.gz 1 8
target/release/examples/count data.nt.gz 4 8
```

## N-Triples behavior

The scanner validates UTF-8, term positions, blank-node labels, absolute IRI
schemes, numeric Unicode escapes, literal escapes, language tags, datatype
suffixes, statement terminators, and trailing content. It supports comments,
blank lines, LF/CR/CRLF, and a final line without a newline. There is no separate
newline pre-scan: the decoder supplies the consumed byte count. UTF-8 validation
checks the consumed record before it is returned.

On x86-64, IRI scanning uses SSE2 to classify 16 bytes at a time after validating
the IRI scheme. Escapes, Unicode, forbidden characters, and short tails use the
scalar scanner. Other architectures use scalar scanning throughout. SIMD is
enabled automatically, requires no CPU-specific build flags, and adds no token
allocations or input padding requirement. Literal and comment scanning remain
scalar.

This is a lexical layer. Values retain their written escapes, language-tag case,
and datatype spelling. It does not unescape, normalize, intern terms, allocate
blank-node identities, or validate every RFC 3987 IRI component. The caller can
add semantic conversion when needed. Passing syntax tests is not a claim of
complete RDF semantic conformance.

The architecture follows the parser ideas in the recovered `DESIGN.md`, using
bounded mappings and borrowing lifetimes in place of a whole-file mapping with
advisory page release. Grammar reference: [W3C RDF 1.1 N-Triples](https://www.w3.org/TR/n-triples/).
File mapping safety reference: [memmap2 MmapOptions](https://docs.rs/memmap2/latest/memmap2/struct.MmapOptions.html).

## SPARQL Query

```rust
use s_parser::parse_query;
use s_parser::sparql::algebra::Algebra;

let query = parse_query(b"PREFIX ex: <http://example.org/>
    SELECT ?s WHERE { ?s a ex:Thing }")?;
let data = query.data();
let root = data.root.unwrap();
if let Algebra::Project(bgp, _vars) = data.node(root) {
    if let Algebra::Bgp(range) = data.node(bgp) {
        assert_eq!(data.triples(range).len(), 1);
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`parse_query` is the common case: one query already in memory, borrowed
directly with no `Stream`/`Reader` involved (there is nothing to window — see
below). It builds one `QueryData` arena and drops it with the `Query` that
owns it — the same build-once-then-drop shape `QuadReader` uses for `Quad`,
just with a heap-allocated tree instead of a fixed-size, stack-resident
`[Token; 4]`, because unlike an N-Triples record (always exactly 4 terms) a
SPARQL algebra tree has no fixed size to be a stack value of. Every span
*within* that tree is still only ever `u32` offsets into the query's own
bytes — nothing here copies query text; see `s_parser::reader::query_reader`'s
doc comment.

For a file of many concatenated queries — a query log or benchmark
corpus — build a `QueryReader<S, T>` over any `Stream`, exactly like
`QuadReader`:

```rust,no_run
use s_parser::{FileStream, QueryReader, SparqlTokenizer};

let stream = unsafe { FileStream::open("queries.rq.log", 1024 * 1024)? };
let mut reader = QueryReader::new(stream, SparqlTokenizer);
while let Some(query) = reader.next()? {
    std::hint::black_box(query.data().form);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

A SPARQL query is self-delimiting by its own grammar — `Prologue`, a query
form, solution modifiers, then an optional `ValuesClause`, and nothing more
belongs to that `QueryUnit` — the same way an N-Triples record is
self-delimited by `.` and a line terminator. `SparqlTokenizer` treats one
`QueryUnit` as one record, so it fits the same `Tokenizer` contract
(`NeedMore` on a truncated window, `window_size` bounding the largest single
query) and the same `Stream` implementations — `FileStream`'s mmap windows and
`GzStream`'s incremental inflation apply here for free. There is no
`for_range`/parallel support: SPARQL property paths and long string literals
can legally contain raw CR/LF, so `SparqlTokenizer` does not implement
`LineTokenizer`, the same reason N-Triples' long-string-free grammar is what
lets it implement that trait at all.

Parsing translates directly into a compact SPARQL algebra tree
(`s_parser::sparql::algebra::Algebra`) rather than an intermediate
concrete-syntax AST — the tokenizer's `Output` for one query, playing the
role `[Token; 4]` plays for one N-Triples record, just variable-size instead
of fixed. Every node (`Algebra`, `Expr`, `PathExpr`) is `Copy` and refers to
its children by index into flat per-kind pools on the query's arena
(`s_parser::sparql::algebra::QueryData`) rather than through `Box`, so a
parsed query is one set of growable `Vec`s, never a tree of individual heap
allocations. Terms (`s_parser::sparql::term::Term`) are spans into the source
text, not references, so resolving a `PREFIX`-declared name never allocates
either — see `Prologue::resolve`. `sparql::Term` and `rdf::Term` are
deliberately different types: one is a position in SPARQL query syntax
(a variable, a prefixed name not yet resolved), the other a decoded RDF term.

Covers SELECT/ASK/CONSTRUCT/DESCRIBE, basic graph patterns, FILTER/OPTIONAL/
UNION/MINUS/BIND/VALUES/GRAPH, subqueries, property paths, the full builtin
function and aggregate vocabulary, and GROUP BY/HAVING/ORDER BY/LIMIT/OFFSET.
`SERVICE` (federated query) and SPARQL Update are not implemented. Aggregates
are hoisted out of `SELECT`/`HAVING`/`ORDER BY` expressions into their
enclosing `Group` node following the algebra translation order in
[§18.2.5](https://www.w3.org/TR/sparql11-query/#sparqlAlgebraEval) of the
spec; `OPTIONAL { P FILTER F }`'s filter becomes `LeftJoin`'s own join
condition rather than a plain nested `Filter`, since it can reference
variables from the left-hand side that a nested `Filter` on `P` alone
wouldn't see. Like N-Triples, this stays a lexical/structural layer:
relative-IRI resolution against `BASE`, `PN_LOCAL` `\`-escape removal, and
grammar-level arity/scope validation beyond what parsing itself requires are
left to the caller.

## Turtle

```rust,no_run
use s_parser::{SliceStream, TurtleReader, TurtleTokenizer};

let src = b"@prefix ex: <http://example.org/> .
    ex:s ex:p ex:o, [ ex:q ex:r ] .";
let mut reader = TurtleReader::new(SliceStream::new(src, 4096)?, TurtleTokenizer::default());
while let Some(statement) = reader.next()? {
    std::hint::black_box(statement.triples());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

One record here is one Turtle *statement* — a directive, or one
`subject predicateObjectList .` after `;`/`,`-list and blank-node/collection
expansion — the same self-delimiting-by-grammar shape a SPARQL `QueryUnit` or
an N-Triples line has. `TurtleTokenizer` treats one statement as one record,
so `TurtleReader` fits the same `Tokenizer` contract and the same `Stream`
implementations as every other reader in this crate, `FileStream`/`GzStream`
included. Like SPARQL, and for the same reason (long string literals and
IRIREFs can legally contain raw CR/LF), `TurtleTokenizer` does not implement
`LineTokenizer`.

A statement can expand, via `;`, `,`, `[...]`, and `(...)`, into many
triples — unlike an N-Triples record (always exactly 4 terms) or a SPARQL
query (one arbitrarily large tree), a statement's triples have no natural
upper bound picked by the grammar itself, and real Turtle (a manifest's
`mf:entries (...)` collection, a long object list) can need far more than a
handful. `s_parser::turtle::term::StatementData` holds up to 16 triples
inline with no allocation — real statements are overwhelmingly this small —
and spills any more into a heap `Vec` rather than erroring, so unlike
`window_size`/`RecordTooLong` there's no fixed cap a legitimate document can
run into. `StatementData` is the tokenizer's `Output`, playing the same
"compact record metadata" role `[Token; 4]` and `QueryData` play for the
other two formats.

Turtle differs from N-Triples and SPARQL in one structural way: `@prefix` is
scoped to the rest of the *document*, not to one statement, and blank-node
identity (`[]`, `_:b`, list/property-list expansion) has to stay distinct
across the whole document too. Neither can live in one statement's own
`StatementData` the way a SPARQL query's `PREFIX`s live in that one query's
`QueryData` — they have to outlive it. So `TurtleTokenizer` itself carries a
small prefix-name table and a blank-node counter across every `decode` call
for as long as the tokenizer lives. Resolving a prefixed name copies its
(short, `@prefix`-bounded) namespace IRI into the term inline
(`s_parser::turtle::term::Namespace`, capped at 64 bytes — a namespace
IRI longer than that is a parse error) rather than borrowing the table or the
declaring statement's own, possibly long-gone, window; the local part stays
an ordinary same-statement span. Between that and the inline/spill triple
buffer above, an ordinary statement — no `@prefix` use, at most 16
triples, which covers the overwhelming majority of real Turtle — allocates
nothing at all, matching N-Triples' guarantee exactly; only an unusually
wide statement or a prefixed name pushes it past that. `turtle::Term` is
structurally `sparql::Term` minus `Var` (Turtle has no variables), but not
the same type, for this exact reason: a SPARQL `Iri::Prefixed` stays an
unresolved span because the whole query (and all its `PREFIX`s) is in memory
together, while Turtle's has to resolve immediately.

Covers directives (`@prefix`/`@base`, and their SPARQL-style `PREFIX`/`BASE`
equivalents), subjects/objects/collections/blank-node property lists,
`a`, and the full literal grammar (typed, language-tagged, numeric, and
boolean shorthands). Like the rest of this crate, this stays a
lexical/structural layer: relative-IRI resolution against `BASE` and
`PN_LOCAL` `\`-escape removal are left to the caller. Grammar reference:
[W3C RDF 1.1 Turtle](https://www.w3.org/TR/turtle/).

## Benchmarking

Performance benchmarking (throughput, memory, plain vs. gzip, serial vs.
parallel) lives in a separate repository,
[`s-bench`](https://github.com/hrssilva/s-bench) — this repository doesn't
depend on, clone, or check against any external RDF test corpus itself.

Reports and inputs from private datasets belong under `private/`, which Git
ignores. Generated public test inputs and compiled artifacts live under
ignored `target/`.

```sh
cargo test --all-targets
cargo test --release --all-targets
cargo test --doc
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Tests cover borrowed views, compact token layout, UTF-8 and escapes, malformed
input, window boundaries, every byte partition, oversized records, offsets above
4 GiB, zero token-storage allocations, and gzip corruption. SIMD differential
tests compare 101,438 mutations and incomplete inputs with the scalar decoder;
Linux x86-64 guard-page tests check vector load bounds. Alternating-window tests
verify overlap, exactly two reused output allocations under slow consumers,
absolute offsets, and worker shutdown on errors or callback panics.
