//! Parallel line parsing with two alternating shared decompression windows.
use crate::{GzStream, LineTokenizer, ParseError, Quad, QuadReader, SliceStream, Stream, Token};
use std::{
    io::{self, Read},
    ops::Range,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// Whole-input counts and stage measurements. Overlapping stage times must not
/// be added to estimate elapsed time.
#[derive(Debug, Default)]
pub struct ParallelReadStats {
    pub triples: u64,
    pub decoded_bytes: u64,
    pub windows: u64,
    /// Sum of refill durations, including compressed input I/O and tail copying.
    pub inflate_time: Duration,
    /// Sum of the slowest worker's parse duration for each window. Excludes
    /// dispatch and coordinator waits; includes the consumer callback.
    pub parse_time: Duration,
    /// Time the coordinator waits for parsers after filling the next window,
    /// including the final drain.
    pub wait_time: Duration,
    pub elapsed: Duration,
}

struct Window {
    bytes: Box<[u8]>,
    len: usize,
    offset: u64,
}

struct Job {
    window: Arc<Window>,
    range: Range<u64>,
}

struct Worker {
    jobs: mpsc::SyncSender<Job>,
    results: mpsc::Receiver<Result<(u64, Duration), ParseError>>,
}

fn rebase(error: ParseError, base: u64) -> ParseError {
    match error {
        ParseError::Syntax { offset, message } => ParseError::Syntax {
            offset: base.saturating_add(offset),
            message,
        },
        ParseError::RecordTooLong { offset, limit } => ParseError::RecordTooLong {
            offset: base.saturating_add(offset),
            limit,
        },
        ParseError::DecoderContract { offset } => ParseError::DecoderContract {
            offset: base.saturating_add(offset),
        },
        other => other,
    }
}

fn parse<T, F>(job: &Job, tokenizer: T, worker: usize, visit: &F) -> Result<u64, ParseError>
where
    T: LineTokenizer<Output = [Token; 4]>,
    F: Fn(usize, Quad<'_>),
{
    let mut reader = QuadReader::for_range(
        SliceStream::new(&job.window.bytes[..job.window.len], job.window.len)?,
        tokenizer,
        job.range.clone(),
    )?;
    let mut count = 0;
    while let Some(quad) = reader.next().map_err(|e| rebase(e, job.window.offset))? {
        visit(worker, quad.with_base(job.window.offset));
        count += 1;
    }
    Ok(count)
}

fn finish(workers: &[Worker], stats: &mut ParallelReadStats) -> Result<(), ParseError> {
    let start = Instant::now();
    let mut slowest = Duration::ZERO;
    // Worker ranges are ordered. Returning the first error in this order keeps
    // diagnostics deterministic even when workers finish in a different order.
    for worker in workers {
        let (count, time) = worker
            .results
            .recv()
            .map_err(|_| io::Error::other("parser worker exited before reporting completion"))??;
        stats.triples += count;
        slowest = slowest.max(time);
    }
    stats.wait_time += start.elapsed();
    stats.parse_time += slowest;
    Ok(())
}

/// Inflate one window while persistent workers parse the other. All workers
/// borrow the same immutable window; only two output buffers are allocated,
/// independent of `readers`. Full windows are never copied.
///
/// `source` must start at decoded offset zero. Each independent window/range
/// gets a clone of `tokenizer`. `visit(worker_index, quad)` receives borrowed
/// records with absolute decoded offsets. Delivery across workers is unordered;
/// copy text explicitly if it must outlive the callback. Callbacks can run
/// concurrently and may already have processed records when an error occurs.
/// A callback panic propagates after all workers shut down.
///
/// The caller thread owns inflation; there are `readers` additional threads.
/// Backpressure waits for all readers before reusing their buffer. Record limits
/// apply to each output window, and consuming the entire input validates gzip
/// trailers. On failure workers are joined before returning; the function cannot
/// cancel a callback or underlying `Read` that itself blocks indefinitely.
pub fn read_gzip_parallel<R, T, F>(
    mut source: GzStream<R>,
    tokenizer: T,
    readers: usize,
    visit: F,
) -> Result<ParallelReadStats, ParseError>
where
    R: Read,
    T: LineTokenizer<Output = [Token; 4]> + Clone + Send,
    F: Fn(usize, Quad<'_>) + Sync,
{
    if readers == 0 || source.offset() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "parallel gzip needs at least one reader and a source at offset zero",
        )
        .into());
    }
    let start = Instant::now();
    let mut shared = Arc::new(Window {
        bytes: vec![0; source.window_size()].into_boxed_slice(),
        len: 0,
        offset: 0,
    });
    let mut stats = ParallelReadStats::default();
    std::thread::scope(|scope| -> Result<(), ParseError> {
        let mut workers = Vec::with_capacity(readers);
        for index in 0..readers {
            let (jobs, requests) = mpsc::sync_channel::<Job>(1);
            // A separate completion channel per worker also disconnects if
            // that worker panics. No coordinator waits forever on a shared
            // channel whose remaining senders belong to idle workers.
            let (results, completed) = mpsc::sync_channel(1);
            workers.push(Worker {
                jobs,
                results: completed,
            });
            let tokenizer = tokenizer.clone();
            let visit = &visit;
            std::thread::Builder::new().spawn_scoped(scope, move || {
                while let Ok(job) = requests.recv() {
                    let begin = Instant::now();
                    let result = parse(&job, tokenizer.clone(), index, visit)
                        .map(|count| (count, begin.elapsed()));
                    drop(job); // Release bytes before signalling completion.
                    if results.send(result).is_err() {
                        break;
                    }
                }
            })?;
        }
        let mut pending = false;
        loop {
            let begin = Instant::now();
            let inflated = source.load(stats.decoded_bytes);
            stats.inflate_time += begin.elapsed();
            // Inflation above overlaps parsing of the previously dispatched
            // window. Report an earlier parse failure before a later I/O error.
            if pending {
                finish(&workers, &mut stats)?;
            }
            inflated?;
            let prefix = if source.is_eof() {
                source.bytes().len()
            } else {
                source
                    .bytes()
                    .iter()
                    .rposition(|b| matches!(b, b'\r' | b'\n'))
                    .map(|i| i + 1)
                    .ok_or(ParseError::RecordTooLong {
                        offset: stats.decoded_bytes,
                        limit: source.window_size(),
                    })?
            };
            if prefix == 0 {
                break;
            }
            let spare = Arc::get_mut(&mut shared).expect("all readers released the window");
            spare.offset = stats.decoded_bytes;
            spare.len = prefix;
            let begin = Instant::now();
            source.exchange_window(&mut spare.bytes, prefix);
            stats.inflate_time += begin.elapsed();
            stats.decoded_bytes += prefix as u64;
            stats.windows += 1;
            for (index, worker) in workers.iter().enumerate() {
                let boundary = |i: usize| (prefix as u128 * i as u128 / readers as u128) as u64;
                worker
                    .jobs
                    .send(Job {
                        window: Arc::clone(&shared),
                        range: boundary(index)..boundary(index + 1),
                    })
                    .map_err(|_| io::Error::other("parser worker exited"))?;
            }
            pending = true;
            if source.is_eof() {
                finish(&workers, &mut stats)?;
                break;
            }
        }
        Ok(())
    })?;
    stats.elapsed = start.elapsed();
    Ok(stats)
}
