use s_parser::*;
use std::{
    fs,
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
};

fn collect(data: &[u8], window: usize) -> Result<Vec<(u64, Vec<u8>)>, ParseError> {
    let mut reader = QuadReader::new(SliceStream::new(data, window).unwrap(), NtTokenizer);
    let mut records = Vec::new();
    while let Some(quad) = reader.next()? {
        records.push((quad.offset(), quad.bytes().to_vec()));
    }
    Ok(records)
}

#[test]
fn token_layout_and_borrowed_values() {
    assert_eq!(std::mem::size_of::<Token>(), 16);
    let input = b"<urn:s> <urn:p> \"a\\n\\\"b\"^^<urn:type> . # note\n";
    let mut reader = QuadReader::new(SliceStream::new(input, 1024).unwrap(), NtTokenizer);
    let quad = reader.next().unwrap().unwrap();
    assert_eq!(quad.subject().kind(), TermType::Iri);
    assert_eq!(quad.subject().value(), b"urn:s");
    assert_eq!(quad.object().kind(), TermType::TypedLiteral);
    assert_eq!(quad.object().text(), b"\"a\\n\\\"b\"^^<urn:type>");
    assert_eq!(quad.object().value(), b"a\\n\\\"b");
    assert_eq!(quad.object().suffix(), b"^^<urn:type>");
    assert!(quad.graph().is_none());
    assert_eq!(quad.bytes().as_ptr(), input.as_ptr());
    assert_eq!(quad.subject().text().as_ptr(), input.as_ptr());
    assert!(reader.next().unwrap().is_none());
}

#[test]
fn language_blank_nodes_and_unicode() {
    let data = "_:é.名 <urn:p> \"Olá 🦀\"@pt-BR .\r\n_:0 <urn:p> _:a.b.\n";
    let mut reader = QuadReader::new(SliceStream::new(data.as_bytes(), 128).unwrap(), NtTokenizer);
    let q = reader.next().unwrap().unwrap();
    assert_eq!(q.subject().value(), "é.名".as_bytes());
    assert_eq!(q.object().value(), "Olá 🦀".as_bytes());
    assert_eq!(q.object().suffix(), b"@pt-BR");
    let q = reader.next().unwrap().unwrap();
    assert_eq!(q.object().value(), b"a.b");
    assert_eq!(q.object().kind(), TermType::BlankNode);
    assert!(reader.next().unwrap().is_none());
}

#[test]
fn valid_syntax_variants() {
    for data in [
        "<urn:s><urn:p><urn:o>.",
        "_:a<urn:p>\"\".\n",
        "<urn:s> <urn:p> \"\\t\\b\\n\\r\\f\\\"\\'\\\\\\u0041\\U0001F980\" .",
        "<\\u0075rn:s> <urn:p> <urn:\\u0061> .",
        "\t <urn:s>\t<urn:p>\t\"x\"@EN-us-123. # comment",
        "<urn:s> <urn:p> _:a..b .\r",
        "<urn:s><urn:p>\"literal with # and .\".\n",
    ] {
        assert_eq!(collect(data.as_bytes(), 256).unwrap().len(), 1, "{data}");
    }
    for data in ["", " \t", "# comment", "\r\n\n\r", "  # αβ\r\n \t\n"] {
        assert!(collect(data.as_bytes(), 64).unwrap().is_empty(), "{data}");
    }
}

#[test]
fn rejects_malformed_records() {
    for data in [
        "<urn:s> <urn:p> <urn:o>",
        "<urn:s> _:p <urn:o> .",
        "\"s\" <urn:p> <urn:o> .",
        "<urn:s> <urn:p> \"unterminated .",
        "<urn:s> <urn:p> \"a\nb\" .",
        "<urn:s> <urn:p> \"bad\\q\" .",
        "<urn:s> <urn:p> \"\\uD800\" .",
        "<urn:s> <urn:p> \"\\U00110000\" .",
        "<urn:s> <urn:p> \"\\uXX00\" .",
        "<urn:s> <urn:p> \"x\"@ .",
        "<urn:s> <urn:p> \"x\"@en- .",
        "<urn:s> <urn:p> \"x\"^^_:a .",
        "<urn:s> <urn:p> \"x\"@en^^<urn:t> .",
        "<urn:s> <urn:p> <urn:o> <urn:g> .",
        "<urn:s> <urn:p> <urn:o> . garbage",
        "<urn:s> <urn:p> <urn:o> . <urn:s><urn:p><urn:o>.",
        "<relative> <urn:p> <urn:o> .",
        "<1urn:s> <urn:p> <urn:o> .",
        "<urn:bad space> <urn:p> <urn:o> .",
        "<urn:s> <urn:p> <urn:bad\\t> .",
        "_: <urn:p> <urn:o> .",
        "_:.a <urn:p> <urn:o> .",
        "_:-a <urn:p> <urn:o> .",
        "_:a. <urn:p> <urn:o> .",
    ] {
        assert!(
            matches!(
                collect(data.as_bytes(), 256),
                Err(ParseError::Syntax { .. })
            ),
            "accepted {data}"
        );
    }
    assert!(collect(b"<urn:s><urn:p>\"\xff\".\n", 64).is_err());
    assert!(collect(b"#\xff\n", 64).is_err());
}

#[test]
fn every_window_size_matches_single_window() {
    let line = "_:é.名 <urn:p> \"Olá \\U0001F980\"@pt-BR .\r\n";
    let data = format!(
        "# header\n\r\n{}# separator\r{}{}",
        line.repeat(9),
        line,
        "<urn:s><urn:p>_:o."
    );
    let expected = collect(data.as_bytes(), data.len()).unwrap();
    for window in line.len()..=data.len() {
        assert_eq!(
            collect(data.as_bytes(), window).unwrap(),
            expected,
            "window {window}"
        );
    }
}

#[test]
fn every_byte_partition_matches_serial() {
    let data = "  # header\n\r\n  _:é <urn:p> \"Olá 🦀\"@pt-BR . # tail\r\n<urn:s><urn:p>_:o.\r_:a<urn:p>\"x\".".as_bytes();
    let expected = collect(data, 64).unwrap();
    for split in 0..=data.len() as u64 {
        let mut actual = Vec::new();
        for range in [0..split, split..data.len() as u64] {
            let mut reader =
                QuadReader::for_range(SliceStream::new(data, 64).unwrap(), NtTokenizer, range)
                    .unwrap();
            while let Some(q) = reader.next().unwrap() {
                actual.push((q.offset(), q.bytes().to_vec()));
            }
        }
        assert_eq!(actual, expected, "partition at {split}");
    }
    // Even partitions far smaller than a single encoded record work.
    let mut actual = Vec::new();
    for start in 0..data.len() as u64 {
        let mut reader = QuadReader::for_range(
            SliceStream::new(data, 64).unwrap(),
            NtTokenizer,
            start..start + 1,
        )
        .unwrap();
        while let Some(q) = reader.next().unwrap() {
            actual.push((q.offset(), q.bytes().to_vec()));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn limits_errors_and_terminal_state() {
    let input = b"<urn:s><urn:p>\"long literal\".\n";
    let mut reader = QuadReader::new(SliceStream::new(input, 16).unwrap(), NtTokenizer);
    assert!(matches!(
        reader.next(),
        Err(ParseError::RecordTooLong {
            offset: 0,
            limit: 16
        })
    ));
    assert!(reader.next().unwrap().is_none());
    assert!(collect(input, input.len()).is_ok());
    assert!(matches!(
        collect(b"# comment too long\n", 4),
        Err(ParseError::RecordTooLong { .. })
    ));
    assert!(SliceStream::new(b"", 0).is_err());
    assert!(QuadReader::for_range(SliceStream::new(b"", 1).unwrap(), NtTokenizer, 0..1).is_err());
    let input = b"<urn:s><urn:p><urn:o>.\n!bad\n";
    assert!(matches!(
        collect(input, 64),
        Err(ParseError::Syntax { offset: 23, .. })
    ));
}

struct TempFile(std::path::PathBuf);
impl TempFile {
    fn new(data: &[u8]) -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "s-parser-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(data).unwrap();
        Self(path)
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

#[test]
fn file_mapping_and_parallel_readers() {
    let data = "<https://example.org/abcdefghijklmnopqrstuvwxyz/subject><urn:p>\"Olá\"@pt.\r\n"
        .repeat(5000);
    let temp = TempFile::new(data.as_bytes());
    // SAFETY: this test owns the file and only removes it after all readers drop.
    let source = unsafe { FileStream::open(&temp.0, 127).unwrap() };
    for workers in [1, 2, 3, 8, 17] {
        let result = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|worker| {
                    let source = source.clone();
                    let range =
                        source.len() * worker / workers..source.len() * (worker + 1) / workers;
                    scope.spawn(move || {
                        let mut reader = QuadReader::for_range(source, NtTokenizer, range).unwrap();
                        let mut offsets = Vec::new();
                        while let Some(q) = reader.next().unwrap() {
                            offsets.push(q.offset());
                        }
                        offsets
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        let expected = collect(data.as_bytes(), 127)
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect::<Vec<_>>();
        assert_eq!(result, expected);
    }
}

#[test]
fn empty_file_and_bounded_mapping() {
    let empty = TempFile::new(b"");
    let source = unsafe { FileStream::open(&empty.0, 64).unwrap() };
    assert!(
        QuadReader::new(source, NtTokenizer)
            .next()
            .unwrap()
            .is_none()
    );
    let temp = TempFile::new(&[b'x'; 10000]);
    let mut source = unsafe { FileStream::open(&temp.0, 113).unwrap() };
    for offset in (0..10000).step_by(107) {
        source.load(offset).unwrap();
        assert!(source.bytes().len() <= 113);
        assert_eq!(source.offset(), offset);
        assert_eq!(source.bytes()[0], b'x');
        assert!(source.clone().bytes().is_empty());
    }
    source.load(10000).unwrap();
    assert!(source.bytes().is_empty());
    assert!(source.load(10001).is_err());
}

#[test]
fn invalid_decoder_cannot_stall_or_expose_out_of_bounds_tokens() {
    struct Bad(bool);
    impl Tokenizer for Bad {
        type Output = [Token; 4];
        fn decode(&mut self, _: &[u8], _: bool) -> Result<Decoded, DecodeError> {
            Ok(if self.0 {
                Decoded {
                    consumed: 0,
                    tokens: None,
                }
            } else {
                Decoded {
                    consumed: 1,
                    tokens: Some([Token::new(TermType::Iri, 0, 10, 11).unwrap(); 4]),
                }
            })
        }
    }
    for bad in [true, false] {
        let mut reader = QuadReader::new(SliceStream::new(b"x", 1).unwrap(), Bad(bad));
        assert!(matches!(
            reader.next(),
            Err(ParseError::DecoderContract { .. })
        ));
        assert!(reader.next().unwrap().is_none());
    }
}

#[test]
fn incomplete_prefixes_request_more_then_parse_at_eof() {
    let data = "_:é<urn:p>\"x\\u0041\"^^<urn:t>. # end\n".as_bytes();
    for end in 0..data.len() {
        assert_eq!(
            NtTokenizer.decode(&data[..end], false).unwrap_err(),
            DecodeError::NeedMore,
            "prefix {end}"
        );
    }
    assert!(NtTokenizer.decode(data, true).unwrap().tokens.is_some());
}

#[test]
fn sparse_file_offsets_above_four_gib() {
    use std::io::{Seek, SeekFrom};
    let temp = TempFile::new(b"");
    let offset = u32::MAX as u64 + 8193;
    let line = b"<urn:s><urn:p><urn:o>.\n";
    {
        let mut file = fs::OpenOptions::new().write(true).open(&temp.0).unwrap();
        file.seek(SeekFrom::Start(offset - 1)).unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(line).unwrap();
    }
    // SAFETY: writes are complete and this test exclusively owns the file.
    let source = unsafe { FileStream::open(&temp.0, 64).unwrap() };
    let end = source.len();
    let mut reader = QuadReader::for_range(source, NtTokenizer, offset..end).unwrap();
    let quad = reader.next().unwrap().unwrap();
    assert_eq!(quad.offset(), offset);
    assert_eq!(quad.bytes(), line);
    assert_eq!(quad.object().value(), b"urn:o");
    assert!(reader.next().unwrap().is_none());
}

#[test]
fn arbitrary_bytes_never_panic_or_stall() {
    let mut seed = 0x1234_5678u32;
    for len in 1..128 {
        for _ in 0..32 {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            let _ = collect(&data, 31);
        }
    }
}

#[test]
fn tokenizer_output_is_not_tied_to_rdf() {
    struct ByteDecoder;
    impl Tokenizer for ByteDecoder {
        type Output = u8;
        fn decode(&mut self, bytes: &[u8], _: bool) -> Result<Decoded<u8>, DecodeError> {
            bytes
                .first()
                .map(|b| Decoded {
                    tokens: Some(*b),
                    consumed: 1,
                })
                .ok_or(DecodeError::NeedMore)
        }
    }
    assert_eq!(ByteDecoder.decode(b"x", true).unwrap().tokens, Some(b'x'));
}
