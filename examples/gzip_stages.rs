//! Measure inflation and parsing separately, with readers sharing one window.
//! Usage: gzip_stages file.nt.gz [readers: 0=inflate only] [window_MiB] [windows: 1|2]
//! One window serializes stages; two windows use the library's overlapping path.
use s_parser::{
    GzStream, NtTokenizer, ParseError, QuadReader, SliceStream, Stream, read_gzip_parallel,
};
use std::{
    error::Error,
    io::{self, Read},
    ops::Range,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Stats {
    bytes: u64,
    triples: u64,
    windows: u64,
    inflate: Duration,
    parse: Duration,
    elapsed: Duration,
}

struct Job<R: Read> {
    source: Arc<GzStream<R>>,
    prefix: usize,
    range: Range<u64>,
}

fn count(bytes: &[u8], range: Range<u64>) -> Result<u64, ParseError> {
    let mut reader = QuadReader::for_range(
        SliceStream::new(bytes, bytes.len().max(1))?,
        NtTokenizer,
        range,
    )?;
    let mut count = 0;
    while let Some(quad) = reader.next()? {
        std::hint::black_box(quad.tokens());
        count += 1;
    }
    Ok(count)
}

fn profile<R: Read + Send + Sync>(
    source: GzStream<R>,
    readers: usize,
) -> Result<Stats, ParseError> {
    let start = Instant::now();
    let mut source = Arc::new(source);
    let mut stats = Stats::default();
    std::thread::scope(|scope| -> Result<(), ParseError> {
        let (results_tx, results_rx) = mpsc::sync_channel(readers.max(1));
        let mut jobs = Vec::new();
        if readers > 1 {
            for _ in 0..readers {
                let (tx, rx) = mpsc::sync_channel::<Job<R>>(1);
                jobs.push(tx);
                let results = results_tx.clone();
                scope.spawn(move || {
                    while let Ok(job) = rx.recv() {
                        let result = count(&job.source.bytes()[..job.prefix], job.range);
                        // Release ownership before completion is observed, so
                        // the coordinator can mutate the window with get_mut.
                        drop(job.source);
                        if results.send(result).is_err() {
                            break;
                        }
                    }
                });
            }
        }
        drop(results_tx);
        loop {
            let begin = Instant::now();
            Arc::get_mut(&mut source)
                .expect("all readers released the previous window")
                .load(stats.bytes)?;
            stats.inflate += begin.elapsed();
            let begin = Instant::now();
            let prefix = if readers == 0 || source.is_eof() {
                source.bytes().len()
            } else {
                // Search only backwards from the end to the last line ending;
                // no full-window newline pre-scan or record-offset allocation.
                source
                    .bytes()
                    .iter()
                    .rposition(|b| matches!(b, b'\r' | b'\n'))
                    .map(|i| i + 1)
                    .ok_or(ParseError::RecordTooLong {
                        offset: stats.bytes,
                        limit: source.window_size(),
                    })?
            };
            if prefix != 0 {
                if readers == 1 {
                    stats.triples += count(&source.bytes()[..prefix], 0..prefix as u64)?;
                } else if readers > 1 {
                    for (worker, jobs) in jobs.iter().enumerate() {
                        let boundary =
                            |i: usize| (prefix as u128 * i as u128 / readers as u128) as u64;
                        jobs.send(Job {
                            source: Arc::clone(&source),
                            prefix,
                            range: boundary(worker)..boundary(worker + 1),
                        })
                        .map_err(|_| io::Error::other("parser worker exited"))?;
                    }
                    for _ in 0..readers {
                        stats.triples += results_rx
                            .recv()
                            .map_err(|_| io::Error::other("parser worker exited"))??;
                    }
                } else {
                    std::hint::black_box(source.bytes());
                }
                stats.windows += 1;
            }
            stats.parse += begin.elapsed();
            stats.bytes += prefix as u64;
            if source.is_eof() {
                break;
            }
        }
        Ok(())
    })?;
    stats.elapsed = start.elapsed();
    Ok(stats)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or(
        "usage: gzip_stages file.nt.gz [readers: 0=inflate only] [window_MiB] [windows: 1|2]",
    )?;
    let readers: usize = args.next().as_deref().unwrap_or("1").parse()?;
    let mib: usize = args.next().as_deref().unwrap_or("8").parse()?;
    let buffers: usize = args.next().as_deref().unwrap_or("1").parse()?;
    if !matches!(buffers, 1 | 2) || (buffers == 2 && readers == 0) {
        return Err("use 1 or 2 windows; two windows require at least one parser reader".into());
    }
    let size = mib.checked_mul(1024 * 1024).ok_or("window size overflow")?;
    if buffers == 2 {
        let stats = read_gzip_parallel(
            GzStream::open(path, size)?,
            NtTokenizer,
            readers,
            |_, quad| {
                std::hint::black_box(quad.tokens());
            },
        )?;
        println!(
            "{{\"readers\":{readers},\"buffer_count\":2,\"window_mib\":{mib},\"decoded_bytes\":{},\"triples\":{},\"windows\":{},\"inflate_seconds\":{:.6},\"parse_seconds\":{:.6},\"wait_seconds\":{:.6},\"elapsed_seconds\":{:.6}}}",
            stats.decoded_bytes,
            stats.triples,
            stats.windows,
            stats.inflate_time.as_secs_f64(),
            stats.parse_time.as_secs_f64(),
            stats.wait_time.as_secs_f64(),
            stats.elapsed.as_secs_f64(),
        );
        return Ok(());
    }
    let stats = profile(GzStream::open(path, size)?, readers)?;
    println!(
        "{{\"readers\":{readers},\"buffer_count\":1,\"window_mib\":{mib},\"decoded_bytes\":{},\"triples\":{},\"windows\":{},\"inflate_seconds\":{:.6},\"parse_seconds\":{:.6},\"elapsed_seconds\":{:.6}}}",
        stats.bytes,
        stats.triples,
        stats.windows,
        stats.inflate.as_secs_f64(),
        stats.parse.as_secs_f64(),
        stats.elapsed.as_secs_f64(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    #[test]
    fn shared_window_partitions_match_serial_across_refills() {
        let line = "<urn:é><urn:p>\"🦀\".\r\n";
        for input in [
            String::new(),
            format!("# header\n{}{}", line.repeat(7), line.trim_end()),
        ] {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(input.as_bytes()).unwrap();
            let compressed = encoder.finish().unwrap();
            for window in [line.len(), line.len() + 1, 64, 1024] {
                for readers in [0, 1, 2, 4, 8] {
                    let stats = profile(
                        GzStream::new(compressed.as_slice(), window).unwrap(),
                        readers,
                    )
                    .unwrap();
                    assert_eq!(stats.bytes, input.len() as u64);
                    assert_eq!(
                        stats.triples,
                        if readers == 0 || input.is_empty() {
                            0
                        } else {
                            8
                        }
                    );
                }
            }
        }
    }
}
