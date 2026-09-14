# s-parser

A Rust parsing core organized around three components:

- `Stream` owns or borrows input bytes and exposes a bounded contiguous window.
- `Reader` lends records to the caller while walking a stream.
- `Tokenizer` produces compact record metadata without owning token text. Its
  associated `Output` type keeps the mechanism independent of RDF.

`QuadReader<S, T>` specializes the reader for four RDF term spans.
`NtTokenizer` handles N-Triples, with the fourth term representing the default
graph. `FileStream` supplies windowed file mappings; `SliceStream` borrows
existing memory. `GzStream` incrementally decompresses gzip files or any compressed
`Read` input. Other RDF syntaxes are not implemented yet.

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

## Validation and reproducible W3C measurements

Public reports use the [W3C RDF test corpus](https://github.com/w3c/rdf-tests).
The RDF 1.1 N-Triples manifest contains 41 positive and 29 negative syntax cases.
Each case is checked with plain input, serial gzip, and four parallel gzip
readers: **210 checks**. These are syntax checks, not full RDF semantic evaluation.

```sh
git clone --depth 1 https://github.com/w3c/rdf-tests.git /tmp/rdf-tests
python3 scripts/check_w3c.py /tmp/rdf-tests
python3 scripts/benchmark_w3c.py /tmp/rdf-tests
```

The benchmark joins the positive fixtures with a newline between files, then
repeats that block to create a larger workload. It measures plain, serial gzip,
and alternating-window gzip parsing without retaining records. This repeated
conformance workload is useful for reproducible comparisons; its size and high
compressibility do not make it representative of arbitrary production graphs.
See [the W3C report](benchmarks/w3c-rdf11.md) for measured times, memory, exact
provenance, and raw results.

Measured on AMD Ryzen 5 PRO 4650U with Radeon Graphics with rustc 1.98.1 (48a229cea 2026-09-01), release compilation,
1 MiB windows, and three runs per configuration. The generated
workload contains **268,441,835 bytes and 2,942,862 triples**;
gzip occupies **10,948,744 bytes**. Times include decompression
where applicable; gzip parser-reader counts exclude the inflater thread.

| Input | Parser readers | Median time | Peak RSS |
|---|---:|---:|---:|
| plain | 1 | 0.510 s | 3.4 MiB |
| plain | 4 | 0.141 s | 6.0 MiB |
| gzip | 1 | 0.554 s | 3.6 MiB |
| gzip | 2 | 0.300 s | 4.5 MiB |
| gzip | 4 | 0.178 s | 4.5 MiB |

Reports and inputs from private datasets belong under `private/`, which Git
ignores. General-purpose benchmark output defaults there. Generated public test
inputs and compiled artifacts live under ignored `target/`.

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
