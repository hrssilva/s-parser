use s_parser::{NtTokenizer, QuadReader, SliceStream};
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
