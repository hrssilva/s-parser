//! Usage: cargo run --release --example count -- data.nt[.gz] [threads] [window_MiB]
use s_parser::{FileStream, GzStream, NtTokenizer, QuadReader, SeekableStream, read_gzip_parallel};
use std::{error::Error, io, time::Instant};

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: count file.nt[.gz] [threads] [window_MiB]")?;
    let threads: usize = args.next().as_deref().unwrap_or("1").parse()?;
    let mib: usize = args.next().as_deref().unwrap_or("8").parse()?;
    if threads == 0 {
        return Err("threads must be positive".into());
    }
    let window = mib.checked_mul(1024 * 1024).ok_or("window size overflow")?;
    let start = Instant::now();
    if std::path::Path::new(&path)
        .extension()
        .is_some_and(|ext| ext == "gz")
    {
        if threads > 1 {
            let stats = read_gzip_parallel(
                GzStream::open(&path, window)?,
                NtTokenizer,
                threads,
                |_, quad| {
                    std::hint::black_box(quad.tokens());
                },
            )?;
            report(
                stats.triples,
                stats.decoded_bytes,
                start.elapsed().as_secs_f64(),
                threads,
                mib,
                "per shared window; 2 windows",
            );
            return Ok(());
        }
        let mut reader = QuadReader::new(GzStream::open(&path, window)?, NtTokenizer);
        let mut count = 0u64;
        while let Some(quad) = reader.next()? {
            std::hint::black_box(quad.tokens());
            count += 1;
        }
        let decoded_bytes = reader.position();
        report(
            count,
            decoded_bytes,
            start.elapsed().as_secs_f64(),
            threads,
            mib,
            "window",
        );
        return Ok(());
    }
    // SAFETY: this example requires an immutable input file for the entire run.
    let source = unsafe { FileStream::open(path, window)? };
    let count = std::thread::scope(|scope| -> Result<u64, Box<dyn Error + Send + Sync>> {
        let handles: Vec<_> = (0..threads)
            .map(|worker| {
                let source = source.clone();
                // u128 intermediate prevents overflow on very large files.
                let boundary =
                    |i: usize| (source.len() as u128 * i as u128 / threads as u128) as u64;
                let range = boundary(worker)..boundary(worker + 1);
                scope.spawn(move || {
                    let mut reader = QuadReader::for_range(source, NtTokenizer, range)?;
                    let mut count = 0u64;
                    while let Some(quad) = reader.next()? {
                        std::hint::black_box(quad.tokens());
                        count += 1;
                    }
                    Ok::<_, s_parser::ParseError>(count)
                })
            })
            .collect();
        let mut count = 0;
        for handle in handles {
            count += handle
                .join()
                .map_err(|_| io::Error::other("worker panicked"))??;
        }
        Ok(count)
    })?;
    report(
        count,
        source.len(),
        start.elapsed().as_secs_f64(),
        threads,
        mib,
        "window/reader",
    );
    Ok(())
}

fn report(count: u64, decoded_bytes: u64, seconds: f64, threads: usize, mib: usize, layout: &str) {
    println!(
        "{count} triples in {seconds:.3}s; {:.1} MiB/s; {:.2} M triples/s; {threads} threads; {mib} MiB {layout}; {decoded_bytes} decoded bytes",
        decoded_bytes as f64 / (1024.0 * 1024.0) / seconds,
        count as f64 / 1e6 / seconds
    );
}
