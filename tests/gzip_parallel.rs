use flate2::{Compression, write::GzEncoder};
use s_parser::*;
use std::{
    io::{self, Read, Write},
    sync::{Mutex, mpsc},
    time::Duration,
};

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

type Record = (u64, [Token; 4], Vec<u8>);

#[test]
fn slow_consumers_share_exactly_two_reused_output_allocations() {
    let line = b"<urn:s><urn:p><urn:o>.\n";
    let window = 4096;
    let prefix = (window / line.len() * line.len()) as u64;
    let compressed = gzip(&line.repeat(2000));
    let allocations = Mutex::new(std::collections::HashSet::new());
    let stats = read_gzip_parallel(
        GzStream::new(compressed.as_slice(), window).unwrap(),
        NtTokenizer,
        8,
        |_, quad| {
            let within_window = (quad.offset() % prefix) as usize;
            allocations
                .lock()
                .unwrap()
                .insert(quad.bytes().as_ptr() as usize - within_window);
            if within_window == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(quad.bytes(), line);
        },
    )
    .unwrap();
    assert_eq!(stats.triples, 2000);
    assert!(stats.windows > 2);
    assert_eq!(allocations.into_inner().unwrap().len(), 2);
}

fn serial(input: &[u8], size: usize) -> Result<Vec<Record>, ParseError> {
    let mut reader = QuadReader::new(SliceStream::new(input, size)?, NtTokenizer);
    let mut records = Vec::new();
    while let Some(q) = reader.next()? {
        records.push((q.offset(), *q.tokens(), q.bytes().to_vec()));
    }
    Ok(records)
}

#[test]
fn alternating_windows_preserve_records_offsets_and_borrowed_tokens() {
    let line = "<urn:é><urn:p>\"🦀\\n\"@pt-BR.\r\n";
    let input = format!("# header\n{}\r{}", line.repeat(13), line.trim_end());
    let compressed = gzip(input.as_bytes());
    let expected = serial(input.as_bytes(), input.len()).unwrap();
    for size in line.len()..=line.len() + 25 {
        for readers in [1, 2, 4, 8] {
            let output = Mutex::new(Vec::new());
            let stats = read_gzip_parallel(
                GzStream::new(compressed.as_slice(), size).unwrap(),
                NtTokenizer,
                readers,
                |index, q| {
                    assert!(index < readers);
                    let start = q.tokens()[0].start();
                    assert_eq!(q.subject().text().as_ptr(), q.bytes()[start..].as_ptr());
                    output
                        .lock()
                        .unwrap()
                        .push((q.offset(), *q.tokens(), q.bytes().to_vec()));
                },
            )
            .unwrap();
            let mut output = output.into_inner().unwrap();
            output.sort_by_key(|r| r.0);
            assert_eq!(output, expected, "window={size}, readers={readers}");
            assert_eq!(stats.triples, expected.len() as u64);
            assert_eq!(stats.decoded_bytes, input.len() as u64);
            assert!(stats.windows > 2);
        }
    }
}

#[test]
fn empty_exact_eof_and_concatenated_members() {
    for input in [
        b"".as_slice(),
        b"# comment",
        b"\r\n",
        b"<urn:s><urn:p><urn:o>.",
    ] {
        for split in 0..=input.len() {
            let compressed = [gzip(&input[..split]), gzip(b""), gzip(&input[split..])].concat();
            let stats = read_gzip_parallel(
                GzStream::new(compressed.as_slice(), input.len().max(1)).unwrap(),
                NtTokenizer,
                4,
                |_, _| {},
            )
            .unwrap();
            assert_eq!(stats.decoded_bytes, input.len() as u64);
            assert_eq!(
                stats.triples,
                serial(input, input.len().max(1)).unwrap().len() as u64
            );
        }
    }
}

#[test]
fn errors_keep_absolute_offsets_and_workers_shutdown() {
    let line = b"<urn:s><urn:p><urn:o>.\n";
    for input in [
        [line.repeat(8), b"!bad\n!also bad\n".to_vec()].concat(),
        [line.repeat(8), vec![b'#'; 100]].concat(),
    ] {
        let expected = format!("{:?}", serial(&input, 48).unwrap_err());
        for readers in [1, 2, 4, 8] {
            let compressed = gzip(&input);
            let error = read_gzip_parallel(
                GzStream::new(compressed.as_slice(), 48).unwrap(),
                NtTokenizer,
                readers,
                |_, _| {},
            )
            .unwrap_err();
            assert_eq!(format!("{error:?}"), expected);
        }
    }
    let compressed = gzip(&line.repeat(40));
    for end in [0, 1, compressed.len() - 1] {
        assert!(matches!(
            read_gzip_parallel(
                GzStream::new(&compressed[..end], 48).unwrap(),
                NtTokenizer,
                4,
                |_, _| {},
            ),
            Err(ParseError::Io(_))
        ));
    }
    let mut corrupt = compressed.clone();
    let crc = corrupt.len() - 8;
    corrupt[crc] ^= 1;
    assert!(matches!(
        read_gzip_parallel(
            GzStream::new(corrupt.as_slice(), 48).unwrap(),
            NtTokenizer,
            4,
            |_, _| {},
        ),
        Err(ParseError::Io(_))
    ));
    assert!(matches!(
        read_gzip_parallel(
            GzStream::new(compressed.as_slice(), 48).unwrap(),
            NtTokenizer,
            0,
            |_, _| {},
        ),
        Err(ParseError::Io(_))
    ));
    let mut advanced = GzStream::new(compressed.as_slice(), 48).unwrap();
    advanced.load(0).unwrap();
    advanced.load(22).unwrap();
    assert!(matches!(
        read_gzip_parallel(advanced, NtTokenizer, 1, |_, _| {}),
        Err(ParseError::Io(_))
    ));
}

#[test]
fn callback_panic_does_not_leave_coordinator_waiting_on_other_workers() {
    let compressed = gzip(&b"<urn:s><urn:p><urn:o>.\n".repeat(100));
    assert!(
        std::panic::catch_unwind(|| {
            let _ = read_gzip_parallel(
                GzStream::new(compressed.as_slice(), 128).unwrap(),
                NtTokenizer,
                4,
                |index, _| {
                    if index == 0 {
                        panic!("intentional consumer panic")
                    }
                },
            );
        })
        .is_err()
    );
}

#[test]
fn inflation_progresses_while_a_consumer_holds_the_other_window() {
    // First determine exactly how many one-byte reads fill the first window.
    // On the next read, rendezvous with a still-running consumer. A serialized
    // implementation cannot complete this handshake and fails by timeout.
    let input: String = (0..200)
        .map(|i| format!("<urn:s{i}><urn:p>\"value{i}\".\n"))
        .collect();
    let compressed = gzip(input.as_bytes());
    struct Probe<'a> {
        input: &'a [u8],
        reads: usize,
    }
    impl Read for Probe<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(1);
            self.reads += 1;
            self.input.read(&mut out[..n])
        }
    }
    let mut probe = Probe {
        input: &compressed,
        reads: 0,
    };
    {
        let mut source = GzStream::new(&mut probe, 256).unwrap();
        source.load(0).unwrap();
    }
    let first_window_reads = probe.reads;
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (advanced_tx, advanced_rx) = mpsc::sync_channel(1);
    struct Gated<'a> {
        probe: Probe<'a>,
        threshold: usize,
        started: mpsc::Receiver<()>,
        advanced: mpsc::SyncSender<()>,
    }
    impl Read for Gated<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.probe.reads == self.threshold {
                self.started
                    .recv_timeout(Duration::from_secs(5))
                    .expect("consumer did not start");
                self.advanced.send(()).unwrap();
            }
            self.probe.read(out)
        }
    }
    let source = GzStream::new(
        Gated {
            probe: Probe {
                input: &compressed,
                reads: 0,
            },
            threshold: first_window_reads,
            started: started_rx,
            advanced: advanced_tx,
        },
        256,
    )
    .unwrap();
    let advanced = Mutex::new(Some(advanced_rx));
    let stats = read_gzip_parallel(source, NtTokenizer, 1, |_, _| {
        if let Some(rx) = advanced.lock().unwrap().take() {
            started_tx.send(()).unwrap();
            rx.recv_timeout(Duration::from_secs(5))
                .expect("inflation did not overlap parsing");
        }
    })
    .unwrap();
    assert_eq!(stats.triples, 200);
    assert_eq!(stats.decoded_bytes, input.len() as u64);
}
