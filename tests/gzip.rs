use flate2::{Compression, GzBuilder, write::GzEncoder};
use s_parser::*;
use std::io::{self, Cursor, Read, Write};

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap()
}

type Record = (u64, [Token; 4], Vec<u8>);

fn records(source: impl Stream) -> Result<Vec<Record>, ParseError> {
    let mut reader = QuadReader::new(source, NtTokenizer);
    let mut output = Vec::new();
    while let Some(q) = reader.next()? {
        output.push((q.offset(), *q.tokens(), q.bytes().to_vec()));
    }
    assert!(reader.next()?.is_none());
    Ok(output)
}

#[test]
fn compressed_records_match_plain_at_every_window_size() {
    let line = "<https://example.org/abcdefghijklmnop/é> <urn:p> \"🦀\\n\"@pt-BR .\r\n";
    let input = format!("# header\n{}\r{}", line.repeat(4), line.trim_end());
    let encoded = gzip(input.as_bytes());
    let expected = records(SliceStream::new(input.as_bytes(), input.len()).unwrap()).unwrap();
    for size in line.len()..=input.len() {
        let actual = records(GzStream::new(encoded.as_slice(), size).unwrap()).unwrap();
        assert_eq!(actual, expected, "window={size}");
    }
}

#[test]
fn exact_window_eof_empty_input_and_comments() {
    for input in [
        b"".as_slice(),
        b"# comment",
        b" \t",
        b"\r\n",
        b"<urn:s><urn:p><urn:o>.",
    ] {
        let encoded = gzip(input);
        let size = input.len().max(1);
        assert_eq!(
            records(GzStream::new(encoded.as_slice(), size).unwrap()).unwrap(),
            records(SliceStream::new(input, size).unwrap()).unwrap(),
        );
        let mut source = GzStream::new(encoded.as_slice(), size).unwrap();
        assert_eq!(source.decoded_len(), None);
        source.load(0).unwrap();
        assert!(source.is_eof());
        assert_eq!(source.decoded_len(), Some(input.len() as u64));
        assert_eq!(source.bytes(), input);
    }
    assert!(GzStream::new(&b""[..], 0).is_err());
}

#[test]
fn concatenated_members_can_split_a_record_or_unicode_character() {
    let input = "<urn:s><urn:p>\"Olá 🦀\".\n".as_bytes();
    let expected = records(SliceStream::new(input, 64).unwrap()).unwrap();
    for split in 0..=input.len() {
        let encoded = [gzip(&input[..split]), gzip(b""), gzip(&input[split..])].concat();
        assert_eq!(
            records(GzStream::new(encoded.as_slice(), 64).unwrap()).unwrap(),
            expected
        );
    }
}

#[test]
fn laziness_sliding_window_and_borrowed_views() {
    use std::{cell::Cell, rc::Rc};
    struct Counted {
        data: Cursor<Vec<u8>>,
        reads: Rc<Cell<usize>>,
    }
    impl Read for Counted {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            self.data.read(output)
        }
    }
    let input = b"<urn:s><urn:p><urn:o>.\n".repeat(100);
    let reads = Rc::new(Cell::new(0));
    let mut source = GzStream::new(
        Counted {
            data: Cursor::new(gzip(&input)),
            reads: reads.clone(),
        },
        64,
    )
    .unwrap();
    assert_eq!(reads.get(), 0);
    assert!(source.bytes().is_empty());
    source.load(0).unwrap();
    assert!(reads.get() > 0);
    assert_eq!(source.bytes(), &input[..64]);
    assert!(!source.is_eof());
    assert_eq!(source.decoded_len(), None);
    let original = source.bytes().as_ptr();
    source.load(21).unwrap();
    assert_eq!(source.bytes().as_ptr(), original);
    assert_eq!(source.bytes(), &input[21..85]);
    assert!(source.load(20).is_err());
    assert!(source.load(86).is_err());
    source.load(21).unwrap();
    assert_eq!(source.bytes(), &input[21..85]);
}

#[test]
fn quad_text_borrows_the_decompression_buffer() {
    let encoded = gzip(b"<urn:s><urn:p><urn:o>.\n");
    let mut source = GzStream::new(encoded.as_slice(), 64).unwrap();
    source.load(0).unwrap();
    let base = source.bytes().as_ptr();
    let mut reader = QuadReader::new(source, NtTokenizer);
    let q = reader.next().unwrap().unwrap();
    assert_eq!(q.bytes().as_ptr(), base);
    assert_eq!(q.subject().text().as_ptr(), base);
}

#[test]
fn one_byte_compressed_reads_and_interrupted_io() {
    struct Choppy {
        data: Cursor<Vec<u8>>,
        interrupt: bool,
    }
    impl Read for Choppy {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.interrupt {
                self.interrupt = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.interrupt = true;
            let len = bytes.len().min(1);
            self.data.read(&mut bytes[..len])
        }
    }
    let input = b"<urn:s><urn:p>\"value\".\n".repeat(5);
    let source = GzStream::new(
        Choppy {
            data: Cursor::new(gzip(&input)),
            interrupt: true,
        },
        32,
    )
    .unwrap();
    assert_eq!(records(source).unwrap().len(), 5);
}

#[test]
fn truncation_corrupt_trailers_and_trailing_garbage_are_errors() {
    let data = b"<urn:s><urn:p><urn:o>.\n";
    let encoded = gzip(data);
    for end in 0..encoded.len() {
        let mut reader = QuadReader::new(GzStream::new(&encoded[..end], 64).unwrap(), NtTokenizer);
        assert!(
            matches!(reader.next(), Err(ParseError::Io(_))),
            "truncation {end}"
        );
        assert!(reader.next().unwrap().is_none());
    }
    for distance in [8, 4] {
        let mut corrupt = encoded.clone();
        let index = corrupt.len() - distance;
        corrupt[index] ^= 1;
        assert!(matches!(
            records(GzStream::new(corrupt.as_slice(), data.len()).unwrap()),
            Err(ParseError::Io(_))
        ));
    }
    let trailing = [encoded.clone(), b"garbage".to_vec()].concat();
    assert!(matches!(
        records(GzStream::new(trailing.as_slice(), 64).unwrap()),
        Err(ParseError::Io(_))
    ));
    let second_truncated = [encoded.clone(), encoded[..encoded.len() - 1].to_vec()].concat();
    assert!(matches!(
        records(GzStream::new(second_truncated.as_slice(), 64).unwrap()),
        Err(ParseError::Io(_))
    ));
    let mut source = GzStream::new(&b"bad gzip"[..], 64).unwrap();
    assert!(source.load(0).is_err());
    assert!(source.load(0).is_err());
}

#[test]
fn record_limits_and_syntax_offsets_apply_to_decoded_bytes() {
    let line = b"<urn:s><urn:p><urn:o>.\n";
    let input = [line.as_slice(), b"!bad\n"].concat();
    let encoded = gzip(&input);
    assert!(matches!(
        records(GzStream::new(encoded.as_slice(), 64).unwrap()),
        Err(ParseError::Syntax { offset: 23, .. })
    ));
    let encoded = gzip(line);
    assert!(matches!(
        records(GzStream::new(encoded.as_slice(), 16).unwrap()),
        Err(ParseError::RecordTooLong {
            offset: 0,
            limit: 16
        })
    ));
}

#[test]
fn optional_header_fields_are_skipped_across_input_buffers() {
    let input = b"<urn:s><urn:p><urn:o>.\n";
    let mut writer = GzBuilder::new()
        .filename(vec![b'n'; 200_000])
        .comment(vec![b'c'; 200_000])
        .extra(vec![b'x'; 60_000])
        .write(Vec::new(), Compression::fast());
    writer.write_all(input).unwrap();
    let encoded = writer.finish().unwrap();
    assert_eq!(
        records(GzStream::new(encoded.as_slice(), 32).unwrap())
            .unwrap()
            .len(),
        1
    );
}
