use s_parser::{
    NtTokenizer, NtWriter, QuadReader, SliceStream, TurtleReader, TurtleTokenizer,
    turtle_to_ntriples,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

struct TrackedAllocator;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
}
#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;
unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK.with(|track| {
            if track.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
                ALLOCATED_BYTES.with(|bytes| bytes.set(bytes.get() + layout.size()));
            }
        });
        // SAFETY: forwards the allocator contract unchanged to System.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: allocations were obtained from System with the same layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[test]
fn gzip_memory_is_independent_of_payload_and_header_length() {
    use flate2::{Compression, GzBuilder};
    use s_parser::GzStream;
    use std::io::Write;

    let line = b"<urn:s><urn:p><urn:o>.\n";
    let mut totals = Vec::new();
    for (header_len, records) in [(0, 100), (2_000_000, 100_000)] {
        let mut encoder = GzBuilder::new()
            .comment(vec![b'x'; header_len])
            .write(Vec::new(), Compression::fast());
        for _ in 0..records {
            encoder.write_all(line).unwrap();
        }
        let compressed = encoder.finish().unwrap();
        ALLOCATED_BYTES.with(|bytes| bytes.set(0));
        TRACK.with(|track| track.set(true));
        let mut reader = QuadReader::new(
            GzStream::new(compressed.as_slice(), 4096).unwrap(),
            NtTokenizer,
        );
        let mut count = 0;
        while let Some(q) = reader.next().unwrap() {
            std::hint::black_box(q.object().value());
            count += 1;
        }
        TRACK.with(|track| track.set(false));
        assert_eq!(count, records);
        totals.push(ALLOCATED_BYTES.with(Cell::get));
    }
    // Count all allocated bytes, a stricter bound than peak live storage. This
    // catches retained gzip headers and allocations repeated for each record.
    assert!(totals[1] < 512 * 1024, "allocations: {totals:?}");
    assert_eq!(totals[0], totals[1]);
}

#[test]
fn parsing_and_window_rollovers_allocate_no_token_storage() {
    let line = b"<https://example.org/abcdefghijklmnopqrstuvwxyz/subject><urn:p>\"a\\n\"^^<https://example.org/abcdefghijklmnopqrstuvwxyz/type>.\n";
    let input = line.repeat(1000);
    let mut reader = QuadReader::new(
        SliceStream::new(&input, line.len() + 13).unwrap(),
        NtTokenizer,
    );
    ALLOCATIONS.with(|count| count.set(0));
    TRACK.with(|track| track.set(true));
    let mut count = 0;
    while let Some(q) = reader.next().unwrap() {
        std::hint::black_box(q.object().value());
        count += 1;
    }
    TRACK.with(|track| track.set(false));
    assert_eq!(count, 1000);
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn turtle_parsing_of_plain_triples_allocates_nothing() {
    // No `@prefix` in sight, so `TurtleTokenizer`'s prefix table never grows
    // past empty, and each statement here has only 2 triples -- well under
    // `StatementData`'s no-allocation inline threshold (16, see
    // `turtle::term::StatementData`), so every triple is written straight
    // into that inline buffer. This should match `NtTokenizer`'s
    // zero-allocation guarantee exactly, not just bound the overhead the
    // way the SPARQL test does.
    let line = b"<https://example.org/abcdefghijklmnopqrstuvwxyz/subject> <urn:p> \"a\\n\"@en ; <urn:q> <urn:o> .\n";
    let input = line.repeat(1000);
    let mut reader = TurtleReader::new(
        SliceStream::new(&input, line.len() + 13).unwrap(),
        TurtleTokenizer::default(),
    );
    ALLOCATIONS.with(|count| count.set(0));
    TRACK.with(|track| track.set(true));
    let mut count = 0;
    while let Some(stmt) = reader.next().unwrap() {
        std::hint::black_box(stmt.triples().len());
        count += 1;
    }
    TRACK.with(|track| track.set(false));
    assert_eq!(count, 1000);
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn turtle_prefixed_names_copy_only_the_bounded_namespace() {
    // One `@prefix` declaration (a handful of small, one-time `Vec` pushes
    // into the tokenizer's own persistent table -- not part of the
    // per-statement cost model) followed by many statements that each
    // *use* the prefix. Each use copies the declared namespace into an
    // inline `Namespace` (see `turtle::term::Namespace`) rather than
    // allocating, so allocator activity should stay flat regardless of how
    // many statements reuse the same prefix.
    let mut input = Vec::from(b"@prefix ex: <http://example.org/> .\n".as_slice());
    let line = b"ex:s ex:p ex:o .\n";
    for _ in 0..1000 {
        input.extend_from_slice(line);
    }
    let mut reader = TurtleReader::new(
        SliceStream::new(&input, 64).unwrap(),
        TurtleTokenizer::default(),
    );
    ALLOCATIONS.with(|count| count.set(0));
    TRACK.with(|track| track.set(true));
    let mut count = 0;
    while let Some(stmt) = reader.next().unwrap() {
        std::hint::black_box(stmt.triples().len());
        count += 1;
    }
    TRACK.with(|track| track.set(false));
    assert_eq!(count, 1000);
    // A handful of allocations for the one `@prefix` declaration itself
    // (the tokenizer's own small `Vec<(Vec<u8>, Vec<u8>)>` table), and
    // nothing that scales with the 1000 statements reusing it.
    assert!(
        ALLOCATIONS.with(Cell::get) <= 4,
        "prefix reuse should not allocate per statement: {} allocations",
        ALLOCATIONS.with(Cell::get)
    );
}

#[test]
fn turtle_statement_wider_than_inline_capacity_spills_exactly_once() {
    // A statement past `StatementData`'s inline-with-no-allocation
    // threshold does allocate -- this is the deliberate trade-off from
    // `turtle_parsing_of_plain_triples_allocates_nothing`'s guarantee, not a
    // bug: the spill should still be one `Vec` allocation (plus its own
    // regrowth as more triples than the initial spill capacity arrive), not
    // one allocation per triple.
    let mut src = String::from("<urn:s>");
    for i in 0..300 {
        src.push_str(&format!(" <urn:p{i}> <urn:o{i}> ;"));
    }
    src.push_str(" <urn:pLast> <urn:oLast> .");
    let mut reader = TurtleReader::new(
        SliceStream::new(src.as_bytes(), src.len() + 16).unwrap(),
        TurtleTokenizer::default(),
    );
    ALLOCATIONS.with(|count| count.set(0));
    TRACK.with(|track| track.set(true));
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples().len();
    TRACK.with(|track| track.set(false));
    assert_eq!(triples, 301);
    assert!(
        ALLOCATIONS.with(Cell::get) <= 8,
        "expected a small, bounded number of spill allocations (`Vec` regrowth), not one per triple: {} allocations for {} triples",
        ALLOCATIONS.with(Cell::get),
        triples
    );
}

#[test]
fn turtle_to_ntriples_conversion_does_not_copy_term_text() {
    // Resolving a term (`Term::resolve`) only ever builds byte-slice
    // references (`ResolvedTerm`/`ResolvedIri` hold `&[u8]`, never `Vec`/
    // `String`), and `NtWriter` streams straight from those into the sink
    // via `write_all` -- no intermediate buffer joins a prefixed IRI's two
    // pieces, and no per-term allocation happens on the resolve side at
    // all. Pre-reserving the output `Vec` rules out its own growth, so the
    // only allocations left should be the *one-time* cost of the single
    // `@prefix` declaration's own table entry (see
    // `turtle_prefixed_names_copy_only_the_bounded_namespace`), not
    // anything that scales with the 500 statements reusing it.
    let line = b"@prefix ex: <http://example.org/a/fairly/long/namespace#> .\n";
    let mut input = Vec::from(line.as_slice());
    let stmt = b"ex:s ex:p ex:o, \"literal value\"@en ; ex:q 42 .\n";
    for _ in 0..500 {
        input.extend_from_slice(stmt);
    }
    let mut reader = TurtleReader::new(
        SliceStream::new(&input, line.len() + stmt.len() + 16).unwrap(),
        TurtleTokenizer::default(),
    );
    let mut out = Vec::with_capacity(1 << 20);
    ALLOCATIONS.with(|count| count.set(0));
    TRACK.with(|track| track.set(true));
    let mut writer = NtWriter::new(&mut out);
    turtle_to_ntriples(&mut reader, &mut writer).unwrap();
    TRACK.with(|track| track.set(false));
    assert!(!out.is_empty());
    assert!(
        ALLOCATIONS.with(Cell::get) <= 4,
        "expected only the one-time @prefix bookkeeping cost, nothing scaling with 500 statements: {} allocations",
        ALLOCATIONS.with(Cell::get)
    );
}

#[test]
fn sparql_parsing_does_not_copy_through_redundant_scratch_buffers() {
    use s_parser::parse_query;
    // A real W3C SPARQL corpus fixture shape (nested FILTER EXISTS), not a
    // contrived worst case. `Term`/`TriplePattern`/`Algebra` are 28/84/64
    // bytes (measured via `size_of`), so this ~250-byte query's actual
    // payload is small; this bounds the *allocator* overhead on top of
    // that, guarding against triples (or similar) round-tripping through
    // more scratch buffers than necessary before reaching their pool --
    // see `Parser::build_triples_node`, which pushes straight into
    // `QueryData::triples` instead of copying through an extra `Vec` first.
    let src = b"prefix ex: <http://www.example.org/>\n\nselect * where {\n  ?s ?p ex:o\n  filter exists { ?s ?p ex:o1  filter exists { ?s ?p ex:o2 } } \n}\n";
    ALLOCATIONS.with(|count| count.set(0));
    ALLOCATED_BYTES.with(|bytes| bytes.set(0));
    TRACK.with(|track| track.set(true));
    std::hint::black_box(parse_query(src).unwrap());
    TRACK.with(|track| track.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    let bytes = ALLOCATED_BYTES.with(Cell::get);
    assert!(
        allocations <= 9 && bytes <= 1800,
        "regression: {allocations} allocations, {bytes} bytes (was 9/1760 after removing the redundant copy)"
    );
}
